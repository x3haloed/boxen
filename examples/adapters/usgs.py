#!/usr/bin/env python3
"""One read-only USGS GeoJSON collection request. Python 3 standard library only."""
import argparse,json,sys,urllib.request,ssl,pathlib
parser=argparse.ArgumentParser();parser.add_argument('--url',default='https://earthquake.usgs.gov/earthquakes/feed/v1.0/summary/all_hour.geojson');args=parser.parse_args()
request=json.load(sys.stdin)
if request.get('protocol')!='boxen-adapter-v1' or request.get('operation')!='collect':raise ValueError('unsupported adapter request')
context=ssl.create_default_context()
# Python.org macOS installations may have no configured CA file. These are OS
# trust bundles; verification and hostname checks remain enabled.
for certificate in ['/etc/ssl/cert.pem','/etc/ssl/certs/ca-certificates.crt']:
    if pathlib.Path(certificate).is_file():context.load_verify_locations(cafile=certificate)
try:
    with urllib.request.urlopen(urllib.request.Request(args.url,headers={'User-Agent':'Boxen-USGS-example/1'}),timeout=4,context=context) as response:
        body=response.read(2*1024*1024+1)
        if len(body)>2*1024*1024:raise ValueError('USGS response exceeds 2 MiB')
        feed=json.loads(body)
except Exception as error:
    reason=getattr(error,'reason',error)
    code='tls_trust' if isinstance(reason,ssl.SSLCertVerificationError) else 'http_'+str(error.code) if hasattr(error,'code') else 'collection_'+type(reason).__name__
    print(json.dumps({'protocol':'boxen-adapter-v1','error':{'code':code}}));sys.exit(0)
if feed.get('type')!='FeatureCollection' or not isinstance(feed.get('features'),list):raise ValueError('unexpected USGS format')
cursor=request.get('cursor') or {};watermark=(cursor.get('updated',0),cursor.get('id',''));observations=[]
# Oldest updates first, at most 32 new/revised reports per collection. Revisions
# remain distinct; a poll does not claim complete history outside the feed window.
for feature in sorted(feed['features'],key=lambda f:(f['properties'].get('updated',0),f['id'])):
    p=feature['properties'];key=str(feature['id'])+':'+str(p.get('updated'))
    point=(p.get('updated',0),feature['id'])
    if point<=watermark:continue
    observations.append({'id':key,'observed_at_ms':int(p['time']),'payload':{'event_id':feature['id'],'updated_at_ms':p.get('updated'),'magnitude':p.get('mag'),'place':p.get('place'),'coordinates':feature.get('geometry',{}).get('coordinates'),'url':p.get('url'),'status':p.get('status'),'coverage':'Reports present in this polling window; updates are distinct observations, not new earthquakes.'}})
    watermark=point
    if len(observations)>=32:break
print(json.dumps({'protocol':'boxen-adapter-v1','observations':observations,'cursor':{'updated':watermark[0],'id':watermark[1]}}))
