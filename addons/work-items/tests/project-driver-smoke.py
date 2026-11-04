#!/usr/bin/env python3
"""Real Windows PowerShell driver + local Git; Azure and SQL effects are native fixtures."""
import argparse,json,os,pathlib,shutil,subprocess,tempfile
parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('--pwsh',required=True,type=pathlib.Path);args=parser.parse_args()
if os.name!='nt':raise SystemExit('This project driver is Windows-specific')
script=pathlib.Path(__file__).resolve().parents[1]/'Project-Driver.ps1'
def run(argv,**kwargs):return subprocess.run(argv,check=True,capture_output=True,text=True,encoding='utf-8',timeout=60,**kwargs).stdout.strip()
with tempfile.TemporaryDirectory(prefix='boxen-project-driver-')as temporary:
 root=pathlib.Path(temporary);repo=root/'repository';repo.mkdir();bare=root/'remote.git';bins=root/'bin';bins.mkdir()
 def git(*parts,cwd=repo):return run(['git','-C',str(cwd),*parts])
 run(['git','init','--bare',str(bare)]);git('init','-b','main');(repo/'source.txt').write_text('base\n');git('add','source.txt');git('-c','user.name=Fixture','-c','user.email=fixture@example.invalid','commit','-m','base');git('remote','add','origin',str(bare));git('push','origin','main')
 fixture=bins/'azure.exe';run(['rustc','--edition','2024',str(pathlib.Path(__file__).with_name('project-driver-fixture.rs')),'-o',str(fixture)]);shutil.copy2(fixture,bins/'sqlcmd.exe')
 env={**os.environ,'PATH':str(bins)+os.pathsep+os.environ['PATH'],'BOXEN_PROJECT_FIXTURE':str(root),'BOXEN_PROJECT_REMOTE':git('remote','get-url','origin')}
 pack=root/'pack';(pack/'scripts').mkdir(parents=True);(pack/'scripts/Inspect-Assignment.ps1').write_text('param($AssignmentPath,$PackRoot)\n\'{"assignment_inputs_valid":true}\'\n')
 project={'repository_root':str(repo),'remote_url':env['BOXEN_PROJECT_REMOTE'],'target_ref':'main','repository_id':'22222222-2222-4222-8222-222222222222','azure_project':'Example','azure_organization':'https://dev.azure.com/example','azure_python':str(fixture),'workspace_root':str(root/'project workspace'),'evidence_root':str(root/'evidence'),'database_prefix':'Synthetic','database_server':'fixture','frontend_port':49001,'backend_port':49002,'browser_context':str(root/'browser'),'tenant_id_base':90000,'ef_tool':str(fixture),'powershell_executable':str(args.pwsh),'login_registration_reference':'Synthetic normal login route','external_dependencies':[],'runtime_process_names':['dotnet','node','WebApp']}
 count=0
 def call(operation,request):
  global count
  count+=1;p=root/f'{count}.json';reply=root/f'{count}.reply.json';p.write_text(json.dumps({'operation':operation,'project':project,'request':request}),encoding='utf-8')
  result=subprocess.run([str(args.pwsh),'-NoProfile','-NonInteractive','-File',str(script),'-RequestPath',str(p),'-ReplyPath',str(reply)],env=env,capture_output=True,text=True,encoding='utf-8',timeout=90)
  value=json.loads(reply.read_text(encoding='utf-8-sig')) if reply.exists()else{}
  assert result.returncode==0,(operation,value,result.stderr);assert value['ok'];return value
 a=call('plan',{'job_id':'a'*64,'attempt':'b'*64,'item':1,'version':1,'previous':None})['assignment'];a.update(job_id='a'*64,work_item={'id':1})
 call('prepare',{'assignment':a,'pack_root':str(pack)});call('prepare',{'assignment':a,'pack_root':str(pack)})
 work=pathlib.Path(a['worktree']);(work/'source.txt').write_text('candidate\n');git('add','source.txt',cwd=work);git('-c','user.name=Fixture','-c','user.email=fixture@example.invalid','commit','-m','candidate',cwd=work)
 candidate={'commit':git('rev-parse','HEAD',cwd=work),'base':a['base_commit'],'summary':'Synthetic candidate','validation':{'acceptance':[{'id':'s1','status':'observed_pass'}],'unverified_gaps':[]}}
 env['BOXEN_PROJECT_BRANCH']=a['branch'];env['BOXEN_PROJECT_COMMIT']=candidate['commit']
 call('verify_cleanup',{'assignment':a,'require_database_removed':True});assert call('inspect_candidate',{'assignment':a,'candidate':candidate})['ready']
 (work/'source.txt').write_text('dirty');assert not call('inspect_candidate',{'assignment':a,'candidate':candidate})['ready'];git('restore','source.txt',cwd=work)
 call('push',{'assignment':a,'candidate':candidate});assert not call('find_pr',{'assignment':a,'candidate':candidate})['prs']
 interpretation={'intended_outcome':'Synthetic behavior','scenarios':[{'action':'Action','expected_result':'Result'}]}
 assert call('create_pr',{'assignment':a,'candidate':candidate,'interpretation':interpretation})['id']==23
 assert call('find_pr',{'assignment':a,'candidate':candidate})['prs'][0]['id']==23
 receipt=call('verify_pr',{'assignment':a,'candidate':candidate,'id':23});assert receipt['commit']==candidate['commit']and receipt['is_draft']is False and receipt['autocomplete']is False
 q={'item':1,'key':'c'*64,'question':'Which visible field is intended?'};assert not call('find_question',q)['found'];call('post_question',q);assert call('find_question',q)['found']
 print(json.dumps({'ok':True,'real_git_worktree_and_push':True,'idempotent_prepare':True,'dirty_candidate_rejected':True,'full_pr_flags_and_multiline_body':True,'remote_receipt':True,'clarification_lookup_and_post':True,'azure_and_sql':'native fixtures, no real service traffic'}))
