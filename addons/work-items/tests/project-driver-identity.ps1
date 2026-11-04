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
