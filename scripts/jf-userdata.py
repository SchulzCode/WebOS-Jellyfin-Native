#!/usr/bin/env python3
"""Save/show/restore Jellyfin watch state (UserData) around test playbacks, so tests leave no trace.
Usage: scripts/jf-userdata.py <session.json> save|show|restore <itemId>...
(session.json: scripts/tvpull /media/developer/jfnative/session.json /tmp/s.json; restore uses <session.json>.userdata.json)"""
import json,sys,urllib.request
s=json.load(open(sys.argv[1])); srv,tok,uid=s['server'],s['token'],s['user_id']
def req(method,path,body=None):
    r=urllib.request.Request(srv+path,method=method,data=None if body is None else json.dumps(body).encode(),
        headers={'Authorization':f'MediaBrowser Token="{tok}"','Content-Type':'application/json'})
    b=urllib.request.urlopen(r,timeout=60).read(); return json.loads(b) if b else None
cmd=sys.argv[2]; ids=sys.argv[3:]
if cmd=='save':
    out={i:req('GET',f'/Users/{uid}/Items/{i}')['UserData'] for i in ids}; json.dump(out,open(sys.argv[1]+'.userdata.json','w'),indent=1); print(json.dumps(out,indent=1))
elif cmd=='show':
    for i in ids: u=req('GET',f'/Users/{uid}/Items/{i}')['UserData']; print(i,{k:u.get(k) for k in ('PlaybackPositionTicks','Played','PlayCount','LastPlayedDate')})
elif cmd=='restore':
    saved=json.load(open(sys.argv[1]+'.userdata.json'))
    for i,u in saved.items():
        body={k:u.get(k) for k in ('PlaybackPositionTicks','Played','PlayCount','LastPlayedDate','IsFavorite') if k in u}
        # Jellyfin ignores LastPlayedDate=null; "mark unplayed" is the only way to clear it (then the saved values go back on)
        if not u.get('LastPlayedDate'): req('DELETE',f'/UserPlayedItems/{i}?userId={uid}'); body.pop('LastPlayedDate',None)
        req('POST',f'/UserItems/{i}/UserData?userId={uid}',body); print('restored',i,body)
