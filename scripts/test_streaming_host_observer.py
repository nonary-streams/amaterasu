import json
from pathlib import Path
import tempfile
import unittest
from streaming_host_observer import RingWriter, counters, numeric_delta, process_identity, sockets

class HostObserverTests(unittest.TestCase):
    def test_identity_handles_spaces_and_parentheses_in_process_name(self):
        self.assertEqual(process_identity('42 (game (worker)) R ' + ' '.join(map(str,range(1,30)))),(42,19))
    def test_only_owned_sockets_are_observed_without_remote_addresses(self):
        header='sl local_address rem_address st tx_queue rx_queue tr tm->when retrnsmt uid timeout inode ref pointer drops\n'
        row='0: 0100007F:BBBC 0100007F:1234 01 00000004:00000008 00:00000000 00000000 1000 0 900 2 00000000 17\n'
        self.assertEqual(sockets(header+row,{'901'}),[])
        value=sockets(header+row,{'900'})[0]
        self.assertEqual(value['receive_queue_bytes'],8)
        self.assertEqual(value['receive_drops_cumulative'],17)
        self.assertNotIn('remote',str(value))
    def test_counter_reset_is_unknown_and_new_threads_are_not_given_fake_deltas(self):
        self.assertEqual(numeric_delta({'x':2,'new':4},{'x':3}),{'x':None})
        self.assertEqual(numeric_delta({'threads':{'2':{'run_ns':7}}},{'threads':{}}),{'threads':{}})
    def test_network_error_fields_are_named_and_protocol_specific(self):
        value=counters('Udp: InDatagrams InErrors RcvbufErrors\nUdp: 5 2 1\n')
        self.assertEqual(value,{'Udp':{'InErrors':2,'RcvbufErrors':1}})
    def test_evidence_rotation_stays_within_the_declared_budget(self):
        with tempfile.TemporaryDirectory() as name:
            path=Path(name)/'observe.jsonl'
            writer=RingWriter(path,160,3)
            for index in range(20):writer.write({'sequence':index,'data':'x'*30})
            writer.close()
            files=list(Path(name).iterdir())
            self.assertLessEqual(len(files),3)
            self.assertTrue(all(p.stat().st_size<=160 for p in files))
            self.assertEqual(json.loads(path.read_text().splitlines()[-1])['sequence'],19)

if __name__=='__main__':unittest.main()
