import json
import os
from pathlib import Path
import socket
import tempfile
import threading
import unittest
from unittest.mock import patch, Mock

from cloud_contacts import recall_client, cli


class OptionalRecallTests(unittest.TestCase):
    def test_missing_service_falls_back_without_creating_files(self):
        with tempfile.TemporaryDirectory() as root, patch.dict(os.environ, XDG_RUNTIME_DIR=root):
            self.assertIsNone(recall_client.rank('test', 'trip', [{'id': 'a'}]))
            self.assertEqual(list(Path(root).iterdir()), [])

    def exchange(self, reply):
        with tempfile.TemporaryDirectory() as root, patch.dict(os.environ, XDG_RUNTIME_DIR=root):
            server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            server.bind(str(Path(root) / 'recall-for-omarchy.sock'))
            server.listen()
            requests = []
            def serve():
                with server.accept()[0] as client:
                    requests.append(json.loads(client.makefile('rb').readline()))
                    client.sendall(json.dumps(reply).encode() + b'\n')
            worker = threading.Thread(target=serve)
            worker.start()
            try:
                result = recall_client.rank('test', 'holiday', [{'id': 'a', 'text': 'travel'}, {'id': 'b', 'text': 'work'}])
            finally:
                worker.join(timeout=2)
                server.close()
            self.assertEqual(requests[0]['api'], 1)
            self.assertEqual(requests[0]['collection'], 'test')
            return result

    def test_valid_scoped_rank(self):
        self.assertEqual(self.exchange({'api': 1, 'meaning': True, 'hits': [{'id': 'a', 'score': .8}]}), ['a'])

    def test_incompatible_service_or_out_of_scope_hits_fall_back(self):
        for reply in ({'api': 2, 'meaning': True, 'hits': []},
                      {'api': 1, 'meaning': True, 'hits': [{'id': 'deleted', 'score': .9}]},
                      {'api': 1, 'error': 'warming up'},
                      {'api': 1, 'meaning': True, 'hits': [{'id': 'a', 'score': 'wrong'}]}):
            with self.subTest(reply=reply):
                self.assertIsNone(self.exchange(reply))

    def test_cli_keeps_native_scope_and_falls_back(self):
        service = Mock()
        service.list.return_value = ([{'id': 'a', 'title': 'holiday', 'name': 'holiday'}], [])
        args = cli.parser().parse_args(['list', '--account', 'work', '--sources', '--semantic', 'holiday'])
        with patch.object(recall_client, 'rank', return_value=None) as rank:
            rows, warnings = cli.dispatch(service, args)
        self.assertEqual([row['id'] for row in rows], ['a'])
        self.assertTrue(warnings)
        self.assertEqual(rank.call_args.args[2][0]['id'], 'a')
        self.assertEqual(service.list.call_args.args, ('holiday', 'work', True))

    def test_normal_listing_never_contacts_recall(self):
        service = Mock()
        service.list.return_value = ([], [])
        with patch.object(recall_client, 'rank', side_effect=AssertionError('unexpected Recall call')):
            cli.dispatch(service, cli.parser().parse_args(['list']))
