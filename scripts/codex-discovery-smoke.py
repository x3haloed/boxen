#!/usr/bin/env python3
"""Windows process-level discovery checks; no real Codex or mailbox traffic."""
import argparse,json,os,pathlib,shutil,subprocess,tempfile,time
parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--boxen',required=True,type=pathlib.Path)
args=parser.parse_args();binary=args.boxen.resolve()
if os.name!='nt': raise SystemExit('This installation-layout smoke targets Windows; portable discovery tests run in cargo test.')
with tempfile.TemporaryDirectory(prefix='boxen-codex-discovery-') as temporary:
    root=pathlib.Path(temporary);desktop=root/'local/OpenAI/Codex/bin';desktop.mkdir(parents=True)
    logs=root/'logs';logs.mkdir()
    fixture=root/'fixture.exe'
    source=pathlib.Path(__file__).resolve().parents[1]/'addons/codex-desktop/tests/queue-fixture.rs'
    subprocess.run(['rustc',str(source),'-o',str(fixture)],check=True,timeout=60)
    env={**os.environ,'LOCALAPPDATA':str(root/'local'),'USERPROFILE':str(root/'home'),'BOXEN_SUPPORT_QUEUE_LOG':str(logs)}
    env.pop('BOXEN_CODEX',None)
    def install(name,version):
        folder=desktop/name;folder.mkdir();shutil.copy2(fixture,folder/'codex.exe');(folder/'version').write_text(version);return folder
    old=install('zzzz','0.166.0-alpha.9');new=install('aaaa','0.166.0-alpha.10');(desktop/'partial-update').mkdir()
    def resolve(*extra):
        process=subprocess.Popen([str(binary),'codex-path',*extra],env=env,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
        stdout,stderr=process.communicate(timeout=8)
        assert process.returncode==0,stderr
        assert not list(pathlib.Path(tempfile.gettempdir()).glob(f'boxen-codex-version-{process.pid}-*.tmp')), 'probe capture leaked'
        return json.loads(stdout)
    assert pathlib.Path(resolve()['executable'])==new/'codex.exe'
    (new/'probe-mode').write_text('sleep');started=time.monotonic()
    assert pathlib.Path(resolve()['executable'])==old/'codex.exe'
    elapsed=time.monotonic()-started;assert .8<elapsed<4,elapsed
    (new/'probe-mode').write_text('flood');started=time.monotonic()
    assert pathlib.Path(resolve()['executable'])==old/'codex.exe'
    assert time.monotonic()-started<4
    (new/'probe-mode').unlink();(new/'version').write_text('0.167.0')
    selected=resolve();assert selected['version']=='codex-cli 0.167.0'
    (new/'codex.exe').unlink()
    assert pathlib.Path(resolve()['executable'])==old/'codex.exe'
    pin=resolve('--codex',str(new/'codex.exe'));assert not pin['automatic'] and pathlib.Path(pin['executable'])==new/'codex.exe'
    print(json.dumps({'ok':True,'numeric_prerelease':True,'partial_update_ignored':True,'probe_timeout_seconds':round(elapsed,3),'output_limit':True,'updated_version_picked_up':True,'disappeared_install_skipped':True,'pin_preserved':True}))
