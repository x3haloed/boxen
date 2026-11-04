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
