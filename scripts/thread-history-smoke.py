#!/usr/bin/env python3
"""Controlled two-daemon regression suite for non-resident cross-thread history (`boxen_threads`).

Verifies:
- Authenticated `thread_history` operator API (`list`, `search`, `read`) across backends.
- Strict resident exclusion: direct residents and promoted residents never appear in `list`/`search`
  and reject `read`; resident activations do not expose or permit `boxen_threads`.
- Ranked BM25 search disambiguation over near-miss distractors, time windows, head+tail budgeting
  with `omitted_middle` pagination, and honest `interrupted` status preservation.
- Live MCP tool access through controlled Codex runs (local + paired peer `machine` selector),
  excluding the calling thread from its own `list`/`search`.
- One-time session-start orientation on run 1 of a new non-resident thread (absent on follow-ups
  and absent on resident activations).
- An ordinary New Thread request receives neither history context nor the model-facing tool
  on a daemon running with default settings.
- Runtime diagnostic logging (`thread_history.operation`) without restart.
"""
import json
import os
import pathlib
import secrets
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from smoke_helpers import fake_codex

ROOT = pathlib.Path(__file__).resolve().parent.parent
BINARY = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else ROOT / 'target/release/boxen').resolve()
COMMAND = (['/bin/bash'] if BINARY.suffix == '.com' else []) + [str(BINARY)]


def port():
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        return sock.getsockname()[1]


def uid():
    return secrets.token_hex(32)


def seed_run(runs_dir, owner, cwd, thread_id, run_id, prompt, assistant, created_at,
             backend='codex', status='completed', after=None, resident=None, tool_output=None):
    spec = {
        'id': run_id,
        'thread_id': thread_id,
        'backend': backend,
        'cwd': cwd,
        'model': 'fixture',
        'prompt': prompt,
    }
    if resident is not None:
        spec['resident_id'] = thread_id
    record = {'spec': spec, 'owner': owner, 'created_at': created_at}
    if after is not None:
        record['after'] = after
    if resident is not None:
        record['resident'] = resident
    run_dir = runs_dir / run_id
    run_dir.mkdir(parents=True, exist_ok=True)
    (run_dir / 'record.json').write_text(json.dumps(record))

    events = [
        {'seq': 1, 'at': created_at, 'kind': 'status', 'data': {'status': 'starting', 'detail': 'Accepted'}},
        {'seq': 2, 'at': created_at + 10, 'kind': 'activation_input' if resident else 'user_message',
         'data': {'text': prompt, 'attachments': []}},
    ]
    seq = 2
    if tool_output is not None:
        seq += 1
        if backend == 'codex':
            events.append({'seq': seq, 'at': created_at + 20, 'kind': 'backend',
                           'data': {'method': 'item/completed', 'params': {'item': {
                               'id': 'cmd1', 'type': 'commandExecution', 'command': 'probe-tool',
                               'aggregatedOutput': tool_output, 'status': 'completed'}}}})
        else:
            events.append({'seq': seq, 'at': created_at + 20, 'kind': 'tool_result',
                           'data': {'call_id': 'c1', 'name': 'probe-tool', 'text': tool_output}})
    if assistant:
        seq += 1
        if backend == 'codex':
            events.append({'seq': seq, 'at': created_at + 30, 'kind': 'backend',
                           'data': {'method': 'item/completed', 'params': {'item': {
                               'id': 'm1', 'type': 'agentMessage', 'phase': 'final_answer', 'text': assistant}}}})
        else:
            events.append({'seq': seq, 'at': created_at + 30, 'kind': 'assistant_message',
                           'data': {'text': assistant}})
    seq += 1
    detail = 'Daemon restarted while command ran; commit status uncertain.' if status == 'interrupted' else 'Done'
    events.append({'seq': seq, 'at': created_at + 40, 'kind': 'status', 'data': {'status': status, 'detail': detail}})
    with (run_dir / 'events.jsonl').open('w') as handle:
        for ev in events:
            handle.write(json.dumps(ev) + '\n')


def step(tool, args, save=None, expect=None, **extra):
    return {'tool': tool, 'args': args, **({'save': save} if save else {}), 'expect': expect or {}, **extra}


with tempfile.TemporaryDirectory(prefix='boxen-thread-history-') as temporary:
    base = pathlib.Path(temporary).resolve()
    homes = [base / 'a', base / 'b']
    ports = [port(), port()]
    uis = [port(), port()]
    workspace = base / 'workspace'
    workspace.mkdir()
    processes = [None, None]
    tokens = [None, None]

    def cli(i, *args):
        return subprocess.check_output(COMMAND + ['--data-dir', str(homes[i]), *args], text=True, timeout=20).strip()

    keys = [cli(i, 'identity') for i in range(2)]
    cli(0, 'trust', keys[1], 'Peer')
    cli(1, 'trust', keys[0], 'Local')

    now_ms = int(time.time() * 1000)
    hour = 3_600_000

    # Seed Daemon A with mixed-backend non-resident threads + direct & promoted resident traps
    runs_a = homes[0] / 'runs'
    t_payout = uid()
    seed_run(runs_a, keys[0], str(workspace), t_payout, t_payout,
             'Webhook HMAC rotation for merchant payouts',
             'Use header X-Payout-Sig-V2 with an 18h grace window.',
             now_ms - 60 * hour, backend='codex')

    t_sandbox = uid()
    seed_run(runs_a, keys[0], str(workspace), t_sandbox, t_sandbox,
             'Webhook HMAC rotation for sandbox staging',
             'Use header X-Sandbox-Sig with a 2h grace window.',
             now_ms - 12 * hour, backend='claude')

    t_interrupted = uid()
    seed_run(runs_a, keys[0], str(workspace), t_interrupted, t_interrupted,
             'Run backfill_v9.sql on production replica',
             '', now_ms - 6 * hour, backend='antigravity', status='interrupted',
             tool_output='BEGIN\nUPDATE batch 1/10...')

    # Long multi-run thread with late reversal to test head+tail budgeting and omitted_middle paging
    t_long = uid()
    prev = None
    for idx in range(14):
        rid = t_long if idx == 0 else uid()
        if idx == 0:
            p, a = 'Tunnel daemon initial setup', 'Tentatively cipher=AES-128-CBC port=7410.'
        elif idx == 13:
            p, a = 'Finalize tunnel daemon settings', 'Final cipher=ChaCha20-Poly1305 port=8443 fallback=8444.'
        else:
            p = f'MTU sweep batch {idx} ' + ('detail ' * 120)
            a = f'Batch {idx} observations: ' + ('log_entry ' * 220)
        seed_run(runs_a, keys[0], str(workspace), t_long, rid, p, a,
                 now_ms - (30 - idx) * hour, backend='openrouter', after=prev,
                 tool_output='probe ' * 400)
        prev = rid

    # Resident traps (MUST NEVER be visible)
    t_res_direct = uid()
    res_obj = {
        'id': t_res_direct, 'owner': keys[0], 'machine_id': keys[0], 'name': 'Private Resident',
        'identity': 'Private', 'memory': 'RESIDENT_CANARY_DIRECT', 'backend': 'codex',
        'cwd': str(workspace), 'model': 'fixture', 'quota_bytes': 65536,
        'auto_activate': False, 'revision': 1, 'created_at': now_ms - 5 * hour,
    }
    seed_run(runs_a, keys[0], str(workspace), t_res_direct, t_res_direct,
             'Private resident Zephyr planning', 'Secret RESIDENT_CANARY_DIRECT',
             now_ms - 5 * hour, resident=res_obj)

    t_res_promoted = uid()
    res_promoted_obj = dict(res_obj, id=t_res_promoted, name='Promoted Resident', memory='RESIDENT_CANARY_PROMOTED')
    seed_run(runs_a, keys[0], str(workspace), t_res_promoted, t_res_promoted,
             'Early anonymous Zephyr brainstorm before promotion', 'Interim RESIDENT_CANARY_PROMOTED',
             now_ms - 4 * hour, resident=None)
    seed_run(runs_a, keys[0], str(workspace), t_res_promoted, uid(),
             'Post-promotion Zephyr checkpoint', 'Locked RESIDENT_CANARY_PROMOTED',
             now_ms - 3 * hour, after=t_res_promoted, resident=res_promoted_obj)

    # Seed Daemon B (paired peer) with a remote non-resident thread + remote resident trap
    runs_b = homes[1] / 'runs'
    t_remote = uid()
    seed_run(runs_b, keys[1], str(workspace), t_remote, t_remote,
             'Remote peer billing export rules',
             'Pipe-delimited files with 42-day Glacier archive retention.',
             now_ms - 8 * 24 * hour, backend='antigravity')
    t_remote_res = uid()
    seed_run(runs_b, keys[1], str(workspace), t_remote_res, t_remote_res,
             'Remote private resident note', 'REMOTE_RESIDENT_CANARY',
             now_ms - 2 * hour, resident=dict(res_obj, id=t_remote_res, owner=keys[1], machine_id=keys[1]))

    def launch(i, *, continuity=False):
        env = os.environ.copy()
        env.pop('BOXEN_EXPERIMENTAL_THREAD_CONTINUITY', None)
        if continuity:
            env['BOXEN_EXPERIMENTAL_THREAD_CONTINUITY'] = '1'
        processes[i] = subprocess.Popen(
            COMMAND + ['--data-dir', str(homes[i]), 'serve', '--listen', f'127.0.0.1:{ports[i]}',
                       '--advertise', f'127.0.0.1:{ports[i]}', '--ui', f'127.0.0.1:{uis[i]}',
                       '--codex', str(fake_codex())],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, env=env)
        lines = [processes[i].stdout.readline() for _ in range(3)]
        assert 'Local controls:' in lines[-1], lines
        tokens[i] = (homes[i] / 'control-token').read_text().strip()

    def stop(i):
        p = processes[i]
        if p and p.poll() is None:
            p.terminate()
            p.wait(timeout=15)
        if p:
            p.stdout.close()
            p.stderr.close()

    def api(i, op):
        req = urllib.request.Request(
            f'http://127.0.0.1:{uis[i]}/api/operation',
            data=json.dumps({'peer': keys[i], 'address': '', 'request': op}).encode(),
            headers={'Authorization': 'Bearer ' + tokens[i], 'Content-Type': 'application/json'})
        with urllib.request.urlopen(req, timeout=30) as resp:
            return json.load(resp)

    def api_rejected(i, op, expected):
        try:
            api(i, op)
        except urllib.error.HTTPError as err:
            body = err.read().decode()
            assert err.code == 422 and expected in body, (err.code, body)
        else:
            raise AssertionError(f'Expected rejection containing {expected!r}')

    def finish(i, run_id):
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            snap = api(i, {'op': 'get', 'id': run_id})
            if snap['status'] in ('completed', 'interrupted', 'failed', 'cancelled'):
                return snap
            time.sleep(0.1)
        raise AssertionError('Run deadline expired')

    try:
        launch(0, continuity=True)
        launch(1)
        cli(0, 'ping', f'127.0.0.1:{ports[1]}', keys[1], 'endpoint discovery')

        # 1. Runtime info advertises boxen_threads under non_resident_tools
        info = api(0, {'op': 'runtime_info'})
        assert 'boxen_threads' in info['agent_work']['non_resident_tools'], info['agent_work']
        default_info = api(1, {'op': 'runtime_info'})
        assert 'boxen_threads' not in default_info['agent_work']['non_resident_tools'], default_info['agent_work']

        # 2. Diagnostic logging check (off by default, runtime enable captures thread_history.operation)
        diag = base / 'thread-history-diag.jsonl'
        api(0, {'op': 'log_file', 'path': str(diag), 'filter': {'level': 'debug', 'targets': ['workload'], 'events': ['thread_history.']}})

        # 3. Direct operator API: list, search, read, head+tail budget, strict resident exclusion
        listed = api(0, {'op': 'thread_history', 'args': {'action': 'list'}})
        listed_ids = [t['thread_id'] for t in listed['threads']]
        assert set(listed_ids) == {t_payout, t_sandbox, t_interrupted, t_long}, listed_ids
        assert t_res_direct not in listed_ids and t_res_promoted not in listed_ids
        assert listed_ids[0] == t_interrupted  # newest first (6h ago)

        # Time window filter: only threads updated within last 24h (excludes 60h t_payout)
        recent = api(0, {'op': 'thread_history', 'args': {'action': 'list', 'since_hours': 24}})
        recent_ids = {t['thread_id'] for t in recent['threads']}
        assert t_payout not in recent_ids and t_interrupted in recent_ids and t_sandbox in recent_ids

        # Search ranking: merchant payout beats more-recent sandbox near-miss; residents never match
        found = api(0, {'op': 'thread_history', 'args': {'action': 'search', 'query': 'merchant payout webhook HMAC'}})
        assert found['results'][0]['thread_id'] == t_payout, found['results']

        res_search = api(0, {'op': 'thread_history', 'args': {'action': 'search', 'query': 'Zephyr RESIDENT_CANARY'}})
        assert res_search['total_matches'] == 0, res_search

        api_rejected(0, {'op': 'thread_history', 'args': {'action': 'read', 'thread_id': t_res_direct}}, 'excluded')
        api_rejected(0, {'op': 'thread_history', 'args': {'action': 'read', 'thread_id': t_res_promoted}}, 'excluded')

        # Interrupted status preservation
        read_int = api(0, {'op': 'thread_history', 'args': {'action': 'read', 'thread_id': t_interrupted}})
        assert read_int['latest_status'] == 'interrupted'
        assert 'uncertain' in read_int['turns'][0]['status_detail']

        # Long thread head+tail budgeting preserves turn 0 and final turn 13 + reports omitted_middle
        read_long = api(0, {'op': 'thread_history', 'args': {'action': 'read', 'thread_id': t_long, 'max_bytes': 8192}})
        turn_indices = [t['turn_index'] for t in read_long['turns']]
        assert turn_indices[0] == 0 and turn_indices[-1] == 13, turn_indices
        assert 'ChaCha20-Poly1305' in read_long['turns'][-1]['assistant']
        assert read_long.get('omitted_middle', {}).get('count', 0) > 0, read_long
        # Paging omitted middle works via start_turn + limit
        page_mid = api(0, {'op': 'thread_history', 'args': {'action': 'read', 'thread_id': t_long, 'start_turn': 3, 'limit': 2}})
        assert [t['turn_index'] for t in page_mid['turns']] == [3, 4], page_mid['turns']

        api(0, {'op': 'log_off'})
        diag_records = [json.loads(line) for line in diag.read_text().splitlines() if line.strip()]
        assert any(r['event'] == 'thread_history.operation' for r in diag_records), diag_records
        print('PASS operator thread_history list/search/read, head+tail paging, resident exclusion, and diagnostics')

        # 4. Live MCP boundary through controlled Codex: local + paired peer + self-exclusion + orientation
        rpc_trace = base / 'rpc-trace.jsonl'
        api(0, {'op': 'log_file', 'path': str(rpc_trace), 'filter': {'level': 'trace', 'events': ['rpc.sent'], 'payloads': True}})
        caller_thread = uid()
        steps = [
            step('boxen_threads', {'action': 'list'}, 'local_list'),
            step('boxen_threads', {'action': 'search', 'query': 'merchant payout webhook'}, 'local_search'),
            step('boxen_threads', {'action': 'read', 'thread_id': t_payout}, 'local_read'),
            step('boxen_threads', {'action': 'read', 'thread_id': t_res_promoted}, error=True),
            step('boxen_threads', {'action': 'search', 'machine': 'Peer', 'query': 'billing export retention'}, 'remote_search'),
            step('boxen_threads', {'action': 'read', 'machine': 'Peer', 'thread_id': t_remote}, 'remote_read'),
            step('boxen_threads', {'action': 'read', 'machine': 'Peer', 'thread_id': t_remote_res}, error=True),
        ]
        api(0, {'op': 'start', 'spec': {
            'id': caller_thread, 'thread_id': caller_thread, 'backend': 'codex',
            'cwd': str(workspace), 'model': 'fixture', 'prompt': 'WORK_INTERFACE:' + json.dumps(steps),
        }})
        snap1 = finish(0, caller_thread)
        assert snap1['status'] == 'completed', snap1
        mcp_out = json.loads((workspace / 'work-interface-results.json').read_text())
        # Calling thread is excluded from its own list/search
        assert caller_thread not in [t['thread_id'] for t in mcp_out['local_list']['threads']]
        assert mcp_out['local_search']['results'][0]['thread_id'] == t_payout
        assert 'X-Payout-Sig-V2' in mcp_out['local_read']['turns'][0]['assistant']
        assert mcp_out['remote_search']['results'][0]['thread_id'] == t_remote
        assert '42-day' in mcp_out['remote_read']['turns'][0]['assistant']

        run2_id = uid()
        api(0, {'op': 'start', 'after': caller_thread, 'spec': {
            'id': run2_id, 'thread_id': caller_thread, 'backend': 'codex',
            'cwd': str(workspace), 'model': 'fixture', 'prompt': 'Follow-up turn',
        }})
        snap2 = finish(0, run2_id)
        assert snap2['status'] == 'completed', snap2

        # 5. Resident activation cannot see or call boxen_threads
        res = api(0, {'op': 'resident_create', 'spec': {
            'id': uid(), 'name': 'Smoke Resident', 'cwd': str(workspace),
            'backend': 'codex', 'model': 'fixture', 'auto_activate': False,
        }})
        act_id = uid()
        act = api(0, {'op': 'resident_activate', 'id': res['id'], 'activation_id': act_id,
                      'directions': 'Ordinary resident check'})
        act_run_id = act['record']['spec']['id']
        act_snap = finish(0, act_run_id)
        assert act_snap['status'] == 'completed', act_snap

        api(0, {'op': 'log_off'})
        rpc_records = [json.loads(l) for l in rpc_trace.read_text().splitlines() if l.strip()]
        def turn_meta(run_id):
            starts = [r for r in rpc_records
                      if r.get('context', {}).get('run_id') == run_id
                      and r.get('fields', {}).get('method') == 'turn/start']
            assert starts, f'missing turn/start for {run_id}'
            return starts[-1]['payload']['params']['input'][1]['text']

        meta1 = turn_meta(caller_thread)
        assert 'Recent non-resident threads on this machine' in meta1, meta1
        assert 'boxen_threads' in meta1, meta1
        assert 'RESIDENT_CANARY' not in meta1, meta1

        meta2 = turn_meta(run2_id)
        assert 'Recent non-resident threads on this machine' not in meta2, meta2
        assert 'boxen_threads' in meta2, meta2
        print('PASS MCP boxen_threads local+peer queries, self-exclusion, and one-time session-start orientation')

        meta_res = turn_meta(act_run_id)
        assert 'boxen_threads' not in meta_res, meta_res
        assert 'Recent non-resident threads on this machine' not in meta_res, meta_res
        res_thread_starts = [r for r in rpc_records
                             if r.get('context', {}).get('run_id') == act_run_id
                             and r.get('fields', {}).get('method') == 'thread/start']
        assert res_thread_starts, 'missing resident thread/start'
        res_dyn_names = [t['name'] for t in res_thread_starts[-1]['payload']['params'].get('dynamicTools', [])]
        assert 'boxen_threads' not in res_dyn_names, res_dyn_names
        print('PASS resident activation excludes boxen_threads and session-start orientation')

        default_trace = base / 'default-rpc-trace.jsonl'
        api(1, {'op': 'log_file', 'path': str(default_trace), 'filter': {'level': 'trace', 'events': ['rpc.sent', 'rpc.received'], 'payloads': True}})
        default_thread = uid()
        api(1, {'op': 'start', 'spec': {
            'id': default_thread, 'thread_id': default_thread, 'backend': 'codex',
            'cwd': str(workspace), 'model': 'fixture',
            'prompt': 'WORK_INTERFACE:' + json.dumps([step('boxen_threads', {'action': 'list'}, error=True)]),
        }})
        default_snap = finish(1, default_thread)
        assert default_snap['status'] == 'completed', default_snap
        api(1, {'op': 'log_off'})
        rpc_records = [json.loads(line) for line in default_trace.read_text().splitlines() if line.strip()]
        default_meta = turn_meta(default_thread)
        assert 'boxen_threads' not in default_meta, default_meta
        assert 'Recent non-resident threads on this machine' not in default_meta, default_meta
        tool_catalogs = [server['tools'] for record in rpc_records
                         for server in record.get('payload', {}).get('result', {}).get('data', [])
                         if server.get('name') == 'boxen_work']
        assert tool_catalogs and all('boxen_threads' not in tools for tools in tool_catalogs), tool_catalogs
        print('PASS default New Thread omits cross-thread context and rejects direct calls to boxen_threads')
    finally:
        stop(0)
        stop(1)
