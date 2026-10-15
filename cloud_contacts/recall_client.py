"""Optional owner-local Recall ranking. Never downloads or starts a model."""
import json
import math
import os
from pathlib import Path
import socket
import time


def rank(collection, query, records, limit=10):
    """Return ranked IDs, or None so the caller retains its existing text search."""
    if not records:
        return []
    runtime = Path(os.environ.get('XDG_RUNTIME_DIR', '/run/user/' + str(os.getuid())))
    deadline = time.monotonic() + 6
    hits = []
    try:
        for start in range(0, len(records), 32):
            batch = records[start:start + 32]
            request = {'api': 1, 'op': 'records.search', 'collection': collection,
                       'query': query, 'records': batch, 'limit': 32}
            payload = json.dumps(request, ensure_ascii=False).encode() + b'\n'
            if len(payload) > 1_048_576:
                return None
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                return None
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
                client.settimeout(remaining)
                client.connect(str(runtime / 'recall-for-omarchy.sock'))
                client.sendall(payload)
                line = b''
                while b'\n' not in line and len(line) <= 1_048_576:
                    remaining = deadline - time.monotonic()
                    if remaining <= 0:
                        return None
                    client.settimeout(remaining)
                    part = client.recv(min(65536, 1_048_577 - len(line)))
                    if not part:
                        return None
                    line += part
                line = line.split(b'\n', 1)[0] + (b'\n' if b'\n' in line else b'')
            if len(line) > 1_048_576 or not line.endswith(b'\n'):
                return None
            reply = json.loads(line)
            if reply.get('api') != 1 or reply.get('error') or reply.get('meaning') is not True:
                return None
            allowed = {record['id'] for record in batch}
            for hit in reply['hits']:
                if hit['id'] not in allowed or not isinstance(hit['score'], (int, float)) or not math.isfinite(hit['score']):
                    return None
                hits.append(hit)
    except (OSError, ValueError, TypeError, KeyError, AttributeError, OverflowError):
        return None
    hits.sort(key=lambda hit: hit['score'], reverse=True)
    return list(dict.fromkeys(hit['id'] for hit in hits))[:limit]


def records(rows):
    return [{'id': row['id'], 'title': str(row.get('name') or row.get('title') or '')[:1000],
             'text': json.dumps(row, ensure_ascii=False, sort_keys=True)[:4000]} for row in rows]
