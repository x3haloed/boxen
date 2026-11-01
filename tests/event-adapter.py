#!/usr/bin/env python3
"""Controlled process boundary failures; never installed in a production source."""
import json,pathlib,sys,time,os
r=json.load(sys.stdin);control=pathlib.Path(sys.argv[1]);mode=control.read_text().strip()
if mode=='effect' and r['operation']=='call':
    control.with_suffix('.pid').write_text(str(os.getpid()))
    effect=control.with_suffix('.effect');effect.write_text(str(int(effect.read_text())+1) if effect.exists() else '1');time.sleep(30)
if mode=='hang':time.sleep(30)
if mode=='flood':sys.stdout.write('x'*100000);sys.stdout.flush();time.sleep(30)
if mode=='fail':raise RuntimeError('controlled broken source')
if mode=='invalid':print('{bad');sys.exit(0)
print(json.dumps({'protocol':'boxen-adapter-v1','observations':[{'id':str(time.time_ns()),'observed_at_ms':int(time.time()*1000),'payload':{'value':mode}}]}))
