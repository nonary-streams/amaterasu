import os
from pathlib import Path
import subprocess
import sys
import time
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parent))
from supervisor import SessionProcess, live_session_members

class SupervisorTest(unittest.TestCase):
    def wait_ready(self, session):
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline and 'ready\n' not in session.output:
            time.sleep(0.02)
        self.assertIn('ready\n', session.output)

    def test_failure_terminates_and_drains_session_without_touching_other_session(self):
        child = "import signal,time; signal.signal(signal.SIGTERM,signal.SIG_IGN); print('ready',flush=True); time.sleep(60)"
        program = f"import subprocess,sys,time; subprocess.Popen([sys.executable,'-c',{child!r}]); time.sleep(60)"
        unrelated = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(60)'], start_new_session=True)
        session = SessionProcess([sys.executable, '-c', program])
        try:
            self.wait_ready(session)
            self.assertGreaterEqual(len(live_session_members(session.session)), 2)
            start = time.monotonic()
            session.close()
            self.assertGreaterEqual(time.monotonic() - start, 1.5)
            self.assertEqual(live_session_members(session.session), [])
            self.assertIsNone(unrelated.poll())
        finally:
            session.close()
            unrelated.terminate()
            unrelated.wait(timeout=5)

    def test_normal_eof_verifies_empty_session(self):
        session = SessionProcess([sys.executable, '-c', "import sys; print('ready',flush=True); sys.stdin.read()"])
        try:
            self.wait_ready(session)
            session.eof()
            session.close()
            self.assertEqual(live_session_members(session.session), [])
        finally:
            session.close()

if __name__ == '__main__':
    unittest.main()
