#!/usr/bin/env python3
"""An external three-by-three game with real persisted moves and stale checks.

Usage: python3 grid-game.py /absolute/path/to/private-game-state.json
The center square is a wall. Start (0,0); goal (2,2). No Boxen game code.
"""
import json,os,pathlib,sys,time
request=json.load(sys.stdin);path=pathlib.Path(sys.argv[1]);path.parent.mkdir(parents=True,exist_ok=True)
# Boxen serializes this source's calls. An independent game server can replace
# this adapter without changing the daemon or its observation/tool contracts.
state=json.loads(path.read_text()) if path.exists() else {'position':[0,0],'revision':1,'calls':{}}
directions={'east':(1,0),'west':(-1,0),'north':(0,-1),'south':(0,1)}
def moves():
    x,y=state['position'];return {d:[x+dx,y+dy] for d,(dx,dy) in directions.items() if 0<=x+dx<3 and 0<=y+dy<3 and [x+dx,y+dy]!=[1,1]}
result={'accepted':True}
if request.get('protocol')!='boxen-adapter-v1':raise ValueError('unsupported protocol')
if request['operation']=='call':
    tool=request['tool'];arguments=request.get('arguments') or {};key=request['call_id']
    if key in state['calls']:result=state['calls'][key]
    elif tool=='move':
        if arguments.get('revision')!=state['revision']:result={'accepted':False,'reason':'stale revision; the accompanying observation describes current state'}
        elif arguments.get('direction') not in moves():result={'accepted':False,'reason':'destination is blocked or outside the board'}
        else:
            state['position']=moves()[arguments['direction']];state['revision']+=1;result={'accepted':True,'revision':state['revision']}
        state['calls'][key]=result
        state['calls']=dict(list(state['calls'].items())[-64:])
    elif tool!='look':raise ValueError('unknown game tool')
elif request['operation']!='collect':raise ValueError('unsupported operation')
temporary=path.with_suffix('.pending');temporary.write_text(json.dumps(state));os.chmod(temporary,0o600);os.replace(temporary,path)
payload={'position':state['position'],'goal':[2,2],'wall':[1,1],'legal_moves':list(moves()),'revision':state['revision'],'won':state['position']==[2,2]}
print(json.dumps({'protocol':'boxen-adapter-v1','result':result,'observations':[{'id':'frame:'+str(state['revision']),'observed_at_ms':int(time.time()*1000),'payload':payload}]}))
