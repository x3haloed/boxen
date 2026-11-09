param([Parameter(Mandatory)][string]$RequestPath,[Parameter(Mandatory)][string]$ReplyPath)
$ErrorActionPreference='Stop'
$inputRecord=Get-Content -LiteralPath $RequestPath -Raw | ConvertFrom-Json -AsHashtable
$project=$inputRecord.project
$request=$inputRecord.request
function Invoke-Git([string]$folder,[string[]]$arguments) {
    $gitProgram=if($project.git_executable){$project.git_executable}else{'git'}
    $lines=& $gitProgram -C $folder @arguments 2>$null
    if($LASTEXITCODE -ne 0){throw 'Git operation failed; no reset, force push or automatic replacement occurred'}
    return ($lines -join "`n").Trim()
}
function Azure([string[]]$arguments) {
    if($project.azure_python){$text=& $project.azure_python -I -B -X utf8 -m azure.cli @arguments --org $project.azure_organization --only-show-errors --output json 2>$null}
    else{$text=& az @arguments --org $project.azure_organization --only-show-errors --output json 2>$null}
    if($LASTEXITCODE -ne 0){throw 'Azure operation failed; an attempted write may have an uncertain outcome'}
    return (($text -join "`n") | ConvertFrom-Json -AsHashtable)
}
function Rooted([string]$root,[string]$path) {
    $base=[IO.Path]::GetFullPath($root).TrimEnd('\','/')+[IO.Path]::DirectorySeparatorChar
    $actual=[IO.Path]::GetFullPath($path)
    if(!$actual.StartsWith($base,[StringComparison]::OrdinalIgnoreCase)){throw 'Assigned path is outside its configured project root'}
    return $actual
}
function Source-Identity {
    if((Invoke-Git $project.repository_root @('remote','get-url','origin')) -cne $project.remote_url){throw 'Repository origin differs from configured identity'}
    if($project.target_ref -notmatch '^[a-zA-Z0-9/_-]+$'){throw 'Target branch is invalid'}
    Invoke-Git $project.repository_root @('fetch','origin',$project.target_ref) | Out-Null
    return Invoke-Git $project.repository_root @('rev-parse',('origin/'+$project.target_ref))
}
function Assigned {
    $a=$request.assignment
    if(!$a){throw 'Assignment missing'}
    $inside=$false
    foreach($root in @($project.workspace_root)+@($project.adopted_workspace_roots)) {
        if(!$root){continue}
        try {Rooted $root $a.worktree | Out-Null;$inside=$true;break} catch {}
    }
    if(!$inside){throw 'Assigned worktree is outside configured workspace roots'}
    Rooted $project.evidence_root $a.evidence_dir | Out-Null
    if($a.repository_root -cne $project.repository_root -or $a.target_ref -cne $project.target_ref){throw 'Assignment project binding differs'}
    return $a
}
function Visual-Evidence($assignment) {
    $visual=$request.candidate.visual_evidence
    $reason=([string]$visual.not_applicable_reason).Trim()
    $screens=@($visual.screenshots | Where-Object {$_})
    if($reason -and !$screens.Count -and $reason.Length -le 1000){
        return @{not_applicable_reason=$reason;screenshots=@()}
    }
    if($reason -or $screens.Count -lt 1 -or $screens.Count -gt 8){
        throw 'Provide a minimal set of screenshots or a specific reason why visual evidence does not apply'
    }
    $directory=Get-Item -LiteralPath $assignment.evidence_dir -Force
    if(!$directory.PSIsContainer -or ($directory.Attributes -band [IO.FileAttributes]::ReparsePoint)){
        throw 'Screenshot evidence directory must be a regular directory'
    }
    $seen=@{}
    $files=@(foreach($screen in $screens){
        $name=[string]$screen.file
        $sha=[string]$screen.sha256
        $caption=([string]$screen.caption).Trim()
        if($name -notmatch '^[A-Za-z0-9][A-Za-z0-9._-]{0,110}\.(png|jpe?g)$' -or
           $sha -cnotmatch '^[0-9a-f]{64}$' -or !$caption -or $caption.Length -gt 500 -or $seen[$sha]){
            throw 'Screenshots need unique SHA-256 hashes, simple JPEG/PNG filenames and concise captions'
        }
        $file=Get-Item -LiteralPath (Join-Path $directory.FullName $name) -Force
        if($file.PSIsContainer -or ($file.Attributes -band [IO.FileAttributes]::ReparsePoint) -or
           $file.Length -lt 8 -or $file.Length -gt 5MB -or
           (Get-FileHash -LiteralPath $file.FullName -Algorithm SHA256).Hash.ToLowerInvariant() -cne $sha){
            throw 'Screenshot must be a regular evidence file of at most 5 MiB with the reported SHA-256'
        }
        $stream=$file.OpenRead()
        try {
            $header=[byte[]]::new(8)
            $stream.ReadExactly($header)
        } finally {$stream.Dispose()}
        $signature=[Convert]::ToHexString($header)
        if(($file.Extension -ieq '.png' -and $signature -cne '89504E470D0A1A0A') -or
           ($file.Extension -ine '.png' -and !$signature.StartsWith('FFD8FF'))){
            throw 'Screenshot content does not match its JPEG/PNG extension'
        }
        $seen[$sha]=$true
        @{path=$file.FullName;sha256=$sha;caption=$caption;extension=$file.Extension.ToLowerInvariant()}
    })
    return @{not_applicable_reason='';screenshots=$files}
}
function Attach-VisualEvidence($visual) {
    if(!$project.azure_python){throw 'PR visual evidence requires the configured Azure CLI Python runtime'}
    $extension=& $project.azure_python -I -B -X utf8 -m azure.cli extension show --name azure-devops --query path --output tsv
    if($LASTEXITCODE -ne 0 -or !$extension){throw 'Azure DevOps CLI extension is unavailable'}
    $spec=@{organization=$project.azure_organization;project=$project.azure_project;repository=$project.repository_id;pr=$request.id;commit=$request.candidate.commit;visual=$visual}
    $specPath=$ReplyPath+'.visual.json'
    $spec | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath $specPath -Encoding utf8
    $code=@'
import hashlib
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, sys.argv[1])
from azext_devops.dev.common.services import get_git_client
from azext_devops.devops_sdk.v5_0.git.models import Comment, GitPullRequestCommentThread

spec = json.loads(Path(sys.argv[2]).read_text(encoding="utf-8-sig"))
git = get_git_client(spec["organization"])
repo, pr, project = spec["repository"], int(spec["pr"]), spec["project"]
commit = spec["commit"]
visual = spec["visual"]
attachments = {a.display_name: a for a in git.get_attachments(repo, pr, project=project)}
lines = [f"### Visual evidence for `{commit[:12]}`"]
names = []
for screenshot in visual["screenshots"]:
    data = Path(screenshot["path"]).read_bytes()
    sha = screenshot["sha256"]
    if len(data) > 5 * 1024 * 1024 or hashlib.sha256(data).hexdigest() != sha:
        raise ValueError("Screenshot changed after validation")
    name = f"visual-{commit[:12]}-{sha[:24]}{screenshot['extension']}"
    if name not in attachments:
        git._send(http_method="POST", location_id="965d9361-878b-413b-a494-45d5b5fd8ab7",
                  version="7.1", route_values={"project": project, "repositoryId": repo,
                  "pullRequestId": str(pr), "fileName": name}, content=data,
                  media_type="application/octet-stream")
        attachments = {a.display_name: a for a in git.get_attachments(repo, pr, project=project)}
    attachment = attachments.get(name)
    if not attachment or attachment.content_hash.lower() != hashlib.sha1(data).hexdigest():
        raise ValueError("PR screenshot attachment readback differs")
    lines.extend([screenshot["caption"], f"![Validation screenshot]({attachment.url})"])
    names.append(name)
if visual["not_applicable_reason"]:
    lines.append("Screenshots are not applicable. " + visual["not_applicable_reason"])
content = "\n\n".join(lines)

def matching_threads():
    return [t for t in git.get_threads(repo, pr, project=project)
            if any(not c.is_deleted and c.content == content for c in t.comments or [])]

matches = matching_threads()
if not matches:
    # Persist intent before posting. A lost response can be reconciled by exact
    # comment readback without posting a second comment during a retry.
    digest = hashlib.sha256(content.encode()).hexdigest()
    intent = Path(sys.argv[2]).with_name(f"visual-pr-{pr}-{digest}.intent")
    try:
        with intent.open("x", encoding="utf-8") as receipt:
            receipt.write(content)
            receipt.flush()
            os.fsync(receipt.fileno())
    except FileExistsError as error:
        raise ValueError("PR evidence comment outcome is uncertain") from error
    git.create_thread(GitPullRequestCommentThread(comments=[
        Comment(parent_comment_id=0, content=content, comment_type="text")],
        status="closed"), repo, pr, project=project)
    matches = matching_threads()
if len(matches) != 1:
    raise ValueError("PR visual evidence comment readback is missing or ambiguous")
print(json.dumps({"ok": True, "verified": True, "thread_id": matches[0].id,
                  "attachments": names, "not_applicable": bool(visual["not_applicable_reason"])}))
'@
    $reply=$code | & $project.azure_python -I -B -X utf8 - $extension $specPath
    if($LASTEXITCODE -ne 0){throw 'PR visual evidence could not be confirmed; retry begins with attachment and comment readback'}
    return (($reply -join "`n") | ConvertFrom-Json -AsHashtable)
}
function Inspect-Candidate {
    $a=Assigned
    $target=Source-Identity
    $candidate=$request.candidate
    if($candidate.commit -notmatch '^[0-9a-f]{40}$' -or $candidate.base -notmatch '^[0-9a-f]{40}$'){throw 'Full candidate commit/base required'}
    if((Invoke-Git $a.worktree @('rev-parse','HEAD')) -cne $candidate.commit){return @{ok=$true;ready=$false;reason='head_changed';target_commit=$target}}
    if((Invoke-Git $a.worktree @('branch','--show-current')) -cne $a.branch){throw 'Candidate checkout is on another branch'}
    if(Invoke-Git $a.worktree @('status','--porcelain')){return @{ok=$true;ready=$false;reason='source_hygiene';target_commit=$target}}
    Invoke-Git $a.worktree @('merge-base','--is-ancestor',$candidate.base,$candidate.commit) | Out-Null
    if($candidate.base -ceq $target -and $project.visual_evidence_required -eq $true){
        try { Visual-Evidence $a | Out-Null }
        catch {return @{ok=$true;ready=$false;reason='visual_evidence';detail=$_.Exception.Message;target_commit=$target}}
    }
    return @{ok=$true;ready=($candidate.base -ceq $target);reason='target_moved';target_commit=$target;commit=$candidate.commit}
}
function Repo-Identity {
    $repo=Azure @('repos','show','--repository',$project.repository_id,'--project',$project.azure_project)
    # Azure may include an organization username in its HTTPS clone URL while Git
    # uses the same URL without userinfo. Compare the complete remaining URI and
    # exact repository ID; passwords and changes to host/path/query remain invalid.
    $actual=[UriBuilder]::new([string]$repo.remoteUrl)
    $expected=[Uri]::new([string]$project.remote_url)
    if($actual.Scheme -ne 'https' -or $expected.Scheme -ne 'https' -or $actual.Password){throw 'Azure repository URL is invalid'}
    $actual.UserName=''
    if($repo.id -cne $project.repository_id -or $actual.Uri.AbsoluteUri -cne $expected.AbsoluteUri){throw 'Azure repository identity differs'}
}
function Publication-Branch($assignment) {
    # A returned assignment may have a new local branch while its previous PR is
    # still active. Update that exact reviewed work instead of opening a duplicate.
    $previous=[string]$assignment.previous_branch
    if(!$previous -or $previous -ceq $assignment.branch){return [string]$assignment.branch}
    if($previous -notmatch '^[a-zA-Z0-9/_-]+$'){throw 'Previous publication branch is invalid'}
    $activePullRequests=@(Azure @('repos','pr','list','--repository',$project.repository_id,'--project',$project.azure_project,'--source-branch',$previous,'--target-branch',$assignment.target_ref,'--status','active'))
    if($activePullRequests.Count -eq 0){return [string]$assignment.branch}
    if($activePullRequests.Count -ne 1){throw 'Previous branch has ambiguous active PRs'}
    $pr=Azure @('repos','pr','show','--id',([string]$activePullRequests[0].pullRequestId))
    $items=@(Azure @('repos','pr','work-item','list','--id',([string]$pr.pullRequestId)))
    if($pr.status -ne 'active' -or $pr.repository.id -cne $project.repository_id -or $pr.sourceRefName -cne ('refs/heads/'+$previous) -or $pr.targetRefName -cne ('refs/heads/'+$assignment.target_ref) -or !($items | Where-Object {[string]$_.id -eq [string]$assignment.work_item.id})){throw 'Previous PR publication binding differs'}
    if($pr.isDraft -or $pr.autoCompleteSetBy){throw 'Previous PR is draft or has autocomplete enabled'}
    return $previous
}
function Sql([string]$query) {
    $sqlProgram=if($project.sqlcmd_executable){$project.sqlcmd_executable}else{'sqlcmd'}
    $text=& $sqlProgram -S $project.database_server -E -d master -b -h -1 -W -Q ('SET NOCOUNT ON; '+$query) 2>$null
    if($LASTEXITCODE -ne 0){throw 'SQL ownership/readiness inspection failed'}
    return ($text -join "`n").Trim()
}
try {
    $result=switch($inputRecord.operation) {
        'plan' {
            $target=Source-Identity
            if($request.job_id -notmatch '^[0-9a-f]{64}$' -or $request.attempt -notmatch '^[0-9a-f]{64}$'){throw 'Stable job/attempt IDs required'}
            $short=$request.job_id.Substring(0,12)
            $worktree=if($request.previous.assignment.worktree){[string]$request.previous.assignment.worktree}else{Join-Path $project.workspace_root ('wi-'+$request.item+'-'+$short)}
            $branch='automation/wi-'+$request.item+'-'+$request.attempt.Substring(0,12)
            $evidence=Join-Path (Join-Path $project.evidence_root $request.job_id) ([string]$request.version)
            $database=$project.database_prefix+'_'+$request.item+'_'+$short
            if($database -notmatch '^[A-Za-z][A-Za-z0-9_]{1,127}$'){throw 'Generated database name invalid'}
            $a=@{repository_root=$project.repository_root;worktree=$worktree;branch=$branch;base_commit=$target;target_ref=$project.target_ref;evidence_dir=$evidence;previous_branch=$request.previous.assignment.branch;runtime=@{required=$true;reservation_id=$request.job_id;allocation_source='Durable automatic project allocation under configured human authority';frontend_port=$project.frontend_port;backend_port=$project.backend_port;database_server=$project.database_server;database_name=$database;synthetic_tenant_id=($project.tenant_id_base+[int]$request.item);ef_tool=$project.ef_tool;browser_context=$project.browser_context;browser_profile_lifecycle='persistent_service_owned';login_registration_reference=$project.login_registration_reference;powershell_executable=$project.powershell_executable;owned_processes=@();external_dependencies=$project.external_dependencies}}
            @{ok=$true;assignment=$a;target_commit=$target}
        }
        'prepare' {
            $a=Assigned
            if(!(Test-Path -LiteralPath $a.worktree)) {
                New-Item -ItemType Directory -Force -Path $project.workspace_root | Out-Null
                Invoke-Git $project.repository_root @('worktree','add','-b',$a.branch,$a.worktree,$a.base_commit) | Out-Null
            } else {
                if((Invoke-Git $a.worktree @('rev-parse','--show-toplevel')).Replace('\','/') -ine ([IO.Path]::GetFullPath($a.worktree)).Replace('\','/')){throw 'Existing path is not its checkout root'}
                $branch=Invoke-Git $a.worktree @('branch','--show-current')
                if($branch -cne $a.branch) {
                    if($branch -cne $a.previous_branch -or (Invoke-Git $a.worktree @('status','--porcelain'))){throw 'Existing worktree needs ownership/dirty-source reconciliation'}
                    Invoke-Git $a.worktree @('switch','-c',$a.branch,$a.base_commit) | Out-Null
                }
            }
            New-Item -ItemType Directory -Force -Path $a.evidence_dir | Out-Null
            $allocationPath=Join-Path (Split-Path $a.evidence_dir) 'allocation.json'
            if(!(Test-Path -LiteralPath $allocationPath)) {
                if((Sql "SELECT COALESCE(CONVERT(varchar(20),DB_ID('$($a.runtime.database_name)')),'absent');") -ne 'absent'){throw 'Generated database already exists without this service allocation marker'}
                @{job_id=$a.job_id;database=$a.runtime.database_name;worktree=$a.worktree} | ConvertTo-Json | Set-Content -LiteralPath $allocationPath -Encoding utf8
            } else {
                $allocation=Get-Content -LiteralPath $allocationPath -Raw | ConvertFrom-Json
                if($allocation.job_id -cne $a.job_id -or $allocation.database -cne $a.runtime.database_name -or $allocation.worktree -cne $a.worktree){throw 'Allocation marker conflicts'}
            }
            $assignmentPath=Join-Path $a.evidence_dir 'assignment.json'
            $a | ConvertTo-Json -Depth 40 | Set-Content -LiteralPath $assignmentPath -Encoding utf8
            $inspection=& $project.powershell_executable -NoProfile -NonInteractive -File (Join-Path $request.pack_root 'scripts/Inspect-Assignment.ps1') -AssignmentPath $assignmentPath -PackRoot $request.pack_root
            if($LASTEXITCODE -ne 0){throw 'Pinned assignment inspector rejected runtime allocation'}
            $inspectionRecord=($inspection -join "`n") | ConvertFrom-Json
            if(!$inspectionRecord.assignment_inputs_valid){throw 'Pinned assignment inputs are invalid'}
            $inspection | Set-Content -LiteralPath (Join-Path $a.evidence_dir 'allocation-inspection.json') -Encoding utf8
            @{ok=$true;prepared=$true;runtime_verified=$false;meaning='Allocation inputs checked; actual runtime reproduction and validation belong to the coder'}
        }
        'verify_cleanup' {
            $a=Assigned
            foreach($port in @($a.runtime.frontend_port,$a.runtime.backend_port)) {
                if(@(Get-NetTCPConnection -State Listen -LocalPort $port -ErrorAction SilentlyContinue).Count){throw 'An allocated application port still has a listener'}
            }
            $processes=Get-CimInstance Win32_Process | Where-Object {$_.CommandLine -and $_.CommandLine.Replace('/','\').Contains($a.worktree.Replace('/','\'),[StringComparison]::OrdinalIgnoreCase) -and ([IO.Path]::GetFileNameWithoutExtension($_.ExecutablePath) -in $project.runtime_process_names)}
            if(@($processes).Count){throw 'Assigned worktree still has an application process'}
            $name=$a.runtime.database_name
            if($name -notmatch '^[A-Za-z][A-Za-z0-9_]{1,127}$'){throw 'Allocated database identifier invalid'}
            $db=Sql "SELECT CASE WHEN DB_ID('$name') IS NULL THEN 'absent' ELSE CONVERT(varchar(20),(SELECT COUNT(*) FROM sys.dm_exec_sessions WHERE database_id=DB_ID('$name'))) END;"
            if($db -ne 'absent' -and $db -ne '0'){throw 'Allocated database has active sessions'}
            if($request.require_database_removed -and $db -ne 'absent'){throw 'Completed candidate retains its allocated database'}
            @{ok=$true;quiescent=$true;database_removed=($db -eq 'absent');browser_profile_retained=$true;application_ports_free=$true;assigned_application_processes=0}
        }
        'inspect_candidate' {Inspect-Candidate}
        'push' {
            $a=Assigned
            $inspection=Inspect-Candidate
            if(!$inspection.ready){throw 'Target moved after candidate validation/review'}
            Repo-Identity
            $publicationBranch=Publication-Branch $a
            # Plain push has server-side fast-forward protection; no force option is present.
            Invoke-Git $a.worktree @('push','origin',($request.candidate.commit+':refs/heads/'+$publicationBranch)) | Out-Null
            $remote=Invoke-Git $a.worktree @('ls-remote','origin',('refs/heads/'+$publicationBranch))
            if(($remote -split '\s+')[0] -cne $request.candidate.commit){throw 'Remote candidate readback differs'}
            @{ok=$true;commit=$request.candidate.commit}
        }
        'find_pr' {
            $a=Assigned;Repo-Identity
            $publicationBranch=Publication-Branch $a
            $prs=@(Azure @('repos','pr','list','--repository',$project.repository_id,'--project',$project.azure_project,'--source-branch',$publicationBranch,'--target-branch',$a.target_ref,'--status','all'))
            @{ok=$true;prs=@($prs | ForEach-Object {@{id=$_.pullRequestId;status=$_.status;is_draft=$_.isDraft}})}
        }
        'create_pr' {
            $a=Assigned;$inspection=Inspect-Candidate
            if(!$inspection.ready){throw 'Target moved before PR creation'}
            Repo-Identity
            $publicationBranch=Publication-Branch $a
            if($publicationBranch -cne $a.branch){throw 'Existing active PR must be reconciled instead of creating a duplicate'}
            $description=([string]$request.candidate.pr_description).Trim()
            if(!$description){throw 'Candidate PR description is missing'}
            $title=([string]$request.interpretation.intended_outcome) -replace '[\r\n]+',' '
            if($title.Length -gt 180){$title=$title.Substring(0,180)}
            $pr=Azure @('repos','pr','create','--repository',$project.repository_id,'--project',$project.azure_project,'--source-branch',$a.branch,'--target-branch',$a.target_ref,'--title',$title,'--description',$description,'--draft','false','--auto-complete','false','--bypass-policy','false','--transition-work-items','false','--work-items',([string]$a.work_item.id),'--detect','false')
            @{ok=$true;id=$pr.pullRequestId}
        }

        'find_question' {
            if($request.key -notmatch '^[0-9a-f]{64}$' -or [int]$request.item -le 0){throw 'Question identity invalid'}
            $marker='[workflow-question:'+$request.key+']';$token=$null;$found=$null
            for($page=0;$page -lt 256;$page++) {
                $arguments=@('devops','invoke','--area','wit','--resource','comments','--api-version','7.1-preview','--route-parameters',('project='+$project.azure_project),('workItemId='+$request.item),'--query-parameters','$top=100','includeDeleted=true')
                if($token){$arguments+=('continuationToken='+$token)}
                $response=Azure $arguments
                $found=$response.comments | Where-Object {!$_.isDeleted -and ([string]$_.text).Contains($marker)} | Select-Object -First 1
                if($found){break}
                $next=$response.continuationToken
                if(!$next){break}
                if($next -eq $token){throw 'Question comment cursor did not advance'}
                $token=$next
                if($page -eq 255){throw 'Question lookup pagination limit exceeded'}
            }
            @{ok=$true;found=[bool]$found;comment_id=$found.id;comment_version=$found.version;service_authored=$true}
        }
        'post_question' {
            if($request.key -notmatch '^[0-9a-f]{64}$' -or [int]$request.item -le 0){throw 'Question identity invalid'}
            $body=@{text=('Automated development workflow question, relaying context-agent analysis (not a human-authored comment):'+"`n`n"+$request.question+"`n`n"+'[workflow-question:'+$request.key+']')}
            $bodyPath=$ReplyPath+'.comment.json';$body | ConvertTo-Json | Set-Content -LiteralPath $bodyPath -Encoding utf8
            $comment=Azure @('devops','invoke','--area','wit','--resource','comments','--api-version','7.1-preview','--route-parameters',('project='+$project.azure_project),('workItemId='+$request.item),'--http-method','POST','--in-file',$bodyPath)
            @{ok=$true;comment_id=$comment.id;comment_version=$comment.version;service_authored=$true}
        }
        {$_ -in 'verify_pr','attach_pr_evidence'} {
            $a=Assigned;Repo-Identity
            $publicationBranch=Publication-Branch $a
            $pr=Azure @('repos','pr','show','--id',([string]$request.id))
            $items=@(Azure @('repos','pr','work-item','list','--id',([string]$request.id)))
            if($pr.repository.id -cne $project.repository_id -or $pr.sourceRefName -cne ('refs/heads/'+$publicationBranch) -or $pr.targetRefName -cne ('refs/heads/'+$a.target_ref)){throw 'PR identity/source/target readback differs'}
            if(!($items | Where-Object {[string]$_.id -eq [string]$a.work_item.id})){throw 'PR work-item link is absent'}
            if($inputRecord.operation -eq 'attach_pr_evidence'){
                if($project.visual_evidence_required -ne $true -or $pr.status -ne 'active' -or
                   $pr.isDraft -or $pr.autoCompleteSetBy -or $pr.lastMergeSourceCommit.commitId -cne $request.candidate.commit){
                    throw 'Active reviewed PR identity is required before attaching visual evidence'
                }
                Attach-VisualEvidence (Visual-Evidence $a)
                break
            }
            @{ok=$true;id=$pr.pullRequestId;url=($project.azure_organization.TrimEnd('/')+'/'+$project.azure_project+'/_git/'+$project.repository_id+'/pullrequest/'+$pr.pullRequestId);is_draft=$pr.isDraft;active=($pr.status -eq 'active');completed=($pr.status -eq 'completed');commit=$pr.lastMergeSourceCommit.commitId;work_item_id=$a.work_item.id;autocomplete=[bool]$pr.autoCompleteSetBy}
        }
        default {throw 'Project operation is unsupported'}
    }
    $result | ConvertTo-Json -Depth 60 | Set-Content -LiteralPath $ReplyPath -Encoding utf8
} catch {
    @{ok=$false;operation=$inputRecord.operation;error=$_.Exception.Message} | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $ReplyPath -Encoding utf8
    exit 1
}
