#!/usr/bin/env python3
"""探测 /api/test_tts 实际播放了几遍：统计 aplay 进程出现次数与时刻。"""
import json
import os
import threading
import time
import urllib.request

pids = {}
stop = threading.Event()


def sample():
    while not stop.is_set():
        try:
            for d in os.listdir('/proc'):
                if not d.isdigit():
                    continue
                try:
                    with open('/proc/%s/comm' % d) as f:
                        if f.read().strip() == 'aplay':
                            pids[d] = time.time()
                except OSError:
                    pass
        except OSError:
            pass
        time.sleep(0.03)


t = threading.Thread(target=sample)
t.start()
t0 = time.time()
req = urllib.request.Request('http://127.0.0.1:8080/api/test_tts',
                             data=b'{}',
                             headers={'Content-Type': 'application/json'})
try:
    resp = json.loads(urllib.request.urlopen(req, timeout=180).read().decode())
except Exception as e:  # noqa: BLE001
    resp = {'err': str(e)}
dt = time.time() - t0
stop.set()
t.join()

seq = sorted(pids.items(), key=lambda kv: kv[1])
print('api_msg  =', resp.get('msg') or resp.get('err'))
print('elapsed  = %.2fs' % dt)
print('aplay 次数 = %d' % len(pids))
for i, (p, ts) in enumerate(seq, 1):
    print('   第%d次 aplay pid=%s  t=+%.2fs' % (i, p, ts - t0))
