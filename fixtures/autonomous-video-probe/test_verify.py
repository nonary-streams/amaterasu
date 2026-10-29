import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("autonomous_video_verify", Path(__file__).with_name("verify.py"))
verify = importlib.util.module_from_spec(spec)
spec.loader.exec_module(verify)

class VerificationTest(unittest.TestCase):
    def test_alternating_pattern_and_rejections(self):
        width, height = 64, 48
        first = bytes([230] * 32 + [20] * 32) * height
        second = bytes([20] * 32 + [230] * 32) * height
        self.assertEqual(verify.verify_pixels(first + second + first, width, height), 3)
        for raw in (first * 3, bytes(width * height * 3), first + b'x'):
            with self.assertRaises(ValueError):
                verify.verify_pixels(raw, width, height)

if __name__ == "__main__":
    unittest.main()
