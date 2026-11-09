$ErrorActionPreference='Stop'
# Exercise the actual driver's identity guard without network or publication.
$source=Join-Path $PSScriptRoot '../Project-Driver.ps1'
$ast=[System.Management.Automation.Language.Parser]::ParseFile($source,[ref]$null,[ref]$null)
$function=$ast.Find({param($node) $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq 'Repo-Identity'},$true)
. ([scriptblock]::Create($function.Extent.Text))
$project=@{repository_id='synthetic-repository';remote_url='https://dev.azure.com/example/project/_git/repository';azure_project='project'}
function Azure {return $script:repository}
foreach($url in @($project.remote_url,'https://example@dev.azure.com/example/project/_git/repository')) {
    $script:repository=@{id=$project.repository_id;remoteUrl=$url}
    Repo-Identity
}
$rejected=0
foreach($case in @(
    @{id='other-repository';remoteUrl=$project.remote_url},
    @{id=$project.repository_id;remoteUrl='https://dev.azure.com/other/project/_git/repository'},
    @{id=$project.repository_id;remoteUrl='https://other.example/example/project/_git/repository'},
    @{id=$project.repository_id;remoteUrl='https://dev.azure.com/example/project/_git/other'},
    @{id=$project.repository_id;remoteUrl=($project.remote_url+'?extra=value')},
    @{id=$project.repository_id;remoteUrl='http://dev.azure.com/example/project/_git/repository'},
    @{id=$project.repository_id;remoteUrl='https://example:synthetic@dev.azure.com/example/project/_git/repository'}
)) {
    $script:repository=$case
    try {Repo-Identity} catch {$rejected++;continue}
    throw 'Changed repository identity was accepted'
}
if($rejected -ne 7){throw 'Missing rejection coverage'}
Write-Output 'PASS repository username normalization and repository/host/path/query/scheme/password guards'

$publicationFunction=$ast.Find({param($node) $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq 'Publication-Branch'},$true)
. ([scriptblock]::Create($publicationFunction.Extent.Text))
$assignment=@{previous_branch='work/previous';branch='work/current';target_ref='development';work_item=@{id=7}}
function Azure([string[]]$arguments) {
    if($arguments[2] -eq 'list'){return $script:activePullRequests}
    if($arguments[2] -eq 'show'){return $script:pullRequest}
    if($arguments[2] -eq 'work-item'){return $script:linkedItems}
    throw 'Unexpected fixture operation'
}
$script:activePullRequests=@()
if((Publication-Branch $assignment) -cne 'work/current'){throw 'Closed/missing previous PR must use current branch'}
$script:activePullRequests=@(@{pullRequestId=42})
$script:pullRequest=@{pullRequestId=42;status='active';repository=@{id=$project.repository_id};sourceRefName='refs/heads/work/previous';targetRefName='refs/heads/development';isDraft=$false;autoCompleteSetBy=$null}
$script:linkedItems=@(@{id=7})
if((Publication-Branch $assignment) -cne 'work/previous'){throw 'Exact active prior PR was not reused'}
foreach($change in @(
    @{status='completed'},@{repository=@{id='other'}},@{sourceRefName='refs/heads/other'},
    @{targetRefName='refs/heads/other'},@{isDraft=$true},@{autoCompleteSetBy=@{id='synthetic'}}
)) {
    $saved=$script:pullRequest.Clone()
    foreach($key in $change.Keys){$script:pullRequest[$key]=$change[$key]}
    $denied=$false
    try {Publication-Branch $assignment | Out-Null} catch {$denied=$true}
    $script:pullRequest=$saved
    if(!$denied){throw 'Changed prior PR binding was accepted'}
}
$script:linkedItems=@(@{id=8});$denied=$false
try {Publication-Branch $assignment | Out-Null} catch {$denied=$true}
if(!$denied){throw 'Wrong work item was accepted'}
$script:activePullRequests=@(@{pullRequestId=42},@{pullRequestId=43});$denied=$false
try {Publication-Branch $assignment | Out-Null} catch {$denied=$true}
if(!$denied){throw 'Ambiguous active PRs were accepted'}
Write-Output 'PASS active prior PR reuse and owner/repository/source/target/work-item/draft/autocomplete guards'

$visualFunction=$ast.Find({param($node) $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq 'Visual-Evidence'},$true)
. ([scriptblock]::Create($visualFunction.Extent.Text))
$directory=[IO.Directory]::CreateTempSubdirectory('boxen-visual-')
$image=Join-Path $directory.FullName 'result.png'
try {
    [IO.File]::WriteAllBytes($image,[Convert]::FromBase64String('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+j1ZkAAAAASUVORK5CYII='))
    $assignment=@{evidence_dir=$directory.FullName}
    $screen=@{file='result.png';caption='The resulting status is displayed';sha256=(Get-FileHash -LiteralPath $image).Hash.ToLowerInvariant()}
    $request=@{candidate=@{visual_evidence=@{screenshots=@($screen)}}}
    if((Visual-Evidence $assignment).screenshots.Count -ne 1){throw 'Valid screenshot was not retained'}
    $request.candidate.visual_evidence=@{not_applicable_reason='This change affects only background retry accounting.'}
    if(!(Visual-Evidence $assignment).not_applicable_reason){throw 'Valid exception was not retained'}
    foreach($invalid in @(
        @{}, @{not_applicable_reason=' '},
        @{screenshots=@($screen);not_applicable_reason='Conflicting evidence forms'},
        @{screenshots=@($screen,$screen)},
        @{screenshots=@(@{file='../result.png';caption=$screen.caption;sha256=$screen.sha256})},
        @{screenshots=@(@{file='result.png';caption=$screen.caption;sha256=('0'*64)})}
    )) {
        $request.candidate.visual_evidence=$invalid
        $rejected=$false
        try {Visual-Evidence $assignment | Out-Null} catch {$rejected=$true}
        if(!$rejected){throw 'Invalid visual evidence was accepted'}
    }
} finally {
    if(Test-Path -LiteralPath $image){Remove-Item -LiteralPath $image}
    Remove-Item -LiteralPath $directory.FullName
}
Write-Output 'PASS screenshot evidence and exception requirements, hashes, duplicates and path bounds'
