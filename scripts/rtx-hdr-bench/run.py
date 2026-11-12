#!/usr/bin/env python3
"""Build and run production native RTX HDR -> NVENC latency comparisons.

Run only while streams are idle. Requires the licensed TrueHDR runtime, NVIDIA
GPU, CUDA headers, nv-codec-headers and EGL/GL/GBM development files.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shlex
import subprocess

ROOT = Path(__file__).resolve().parents[2]


def run(command, **kwargs):
    return subprocess.run(command, check=True, text=True, **kwargs)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--runtime', type=Path, required=True)
    parser.add_argument('--build-dir', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--compare', type=Path, help='compare against a saved report with identical settings')
    parser.add_argument('--max-regression-percent', type=float, default=20)
    parser.add_argument('--self-test', action='store_true', help='also prove the real-code harness detects an injected 5 ms completion delay')
    parser.add_argument('--snapshot', action='store_true', help='include production VKMS immutable snapshot copy')
    parser.add_argument('--peak', type=int, default=1424)
    parser.add_argument('--width', type=int, default=2560)
    parser.add_argument('--height', type=int, default=1440)
    parser.add_argument('--frames', type=int, default=240)
    parser.add_argument('--fps', type=int, default=120, help='0 runs without pacing')
    args = parser.parse_args()
    if not 400 <= args.peak <= 2000 or not 1 <= args.frames <= 100000 or not 0 <= args.fps <= 1000:
        parser.error('invalid peak, frame count or FPS')
    if min(args.width,args.height) < 2 or args.width % 2 or args.height % 2:
        parser.error('dimensions must be positive even values')
    environment = dict(os.environ, AMATERASU_BENCH_PEAK=str(args.peak))
    if args.snapshot:
        environment['AMATERASU_BENCH_SNAPSHOT']='1'
    else:
        environment.pop('AMATERASU_BENCH_SNAPSHOT',None)
    build = args.build_dir.resolve()
    build.mkdir(parents=True, exist_ok=True)
    rtx = ROOT / 'modules/rtx-hdr/src/native'
    snapshot = ROOT / 'modules/capture-vkms/capture/src/platform'
    encode = ROOT / 'modules/encode-video/encode/src/platform/nvenc_cuda/native'
    for source, header, symbol in [(rtx/'cuda/output.ptx', 'rtx_output_ptx.h', 'am_rtx_output_ptx'),
                                    (encode/'cuda/convert.ptx', 'nvenc_convert_ptx.h', 'am_nvenc_convert_ptx')]:
        data = source.read_bytes() + b'\0'
        (build/header).write_text('static const unsigned char '+symbol+'[] = {'+','.join(map(str,data))+'};\n')
    flags = shlex.split(subprocess.check_output(['pkg-config','--cflags','--libs','egl','gl','gbm','libdrm','glesv2'], text=True))
    includes = ['-I'+str(snapshot), '-I'+str(build), '-I'+str(rtx), '-I'+str(encode), '-I'+str(ROOT/'sdk/include'),
                '-I'+os.environ.get('AMATERASU_CUDA_INCLUDE_DIR','/opt/cuda/include'),
                '-I'+os.environ.get('AMATERASU_NVENC_INCLUDE_DIR','/usr/include/ffnvcodec')]
    sources = [encode/(s+'.c') for s in ['loader','cuda_input','display','convert','nvenc','test_support']]
    sources.append(snapshot/'gpu_snapshot.c')
    objects = []
    for source in sources:
        obj=build/(source.stem+'.o'); objects.append(str(obj))
        run(['cc','-O2','-std=c11','-D_POSIX_C_SOURCE=200809L',*includes,*flags,'-c',str(source),'-o',str(obj)])
    cpp = [rtx/(s+'.cpp') for s in ['runtime','surface','effect','timing','cuda_output','test_support']]
    executable = build/'bench'
    run(['c++','-O2','-std=c++17','-pthread',*includes,str(Path(__file__).with_name('main.cpp')),
         *map(str,cpp),*objects,*flags,'-ldl','-o',str(executable)])
    results = []
    # Reverse order on the second pair to reduce warm-up/clock-order bias.
    for mode in ['historical','current','current','historical','bypass']:
        result = run([str(executable),str(args.runtime.resolve()),mode,str(args.width),str(args.height),
                      str(args.frames),str(args.fps)],capture_output=True,env=environment,timeout=180)
        sample=json.loads(result.stdout); results.append(sample)
        print(json.dumps(sample), flush=True)
    self_test = None
    if args.self_test:
        probes = []
        for delay in [0,5000,5000,0]:
            measured = run([str(executable),str(args.runtime.resolve()),'current','640','360','60','60',str(delay)],capture_output=True,env=environment,timeout=60)
            probes.append(json.loads(measured.stdout))
        normal = sum(p['source_to_bitstream']['p50_us'] for p in probes if p['injected_completion_delay_us','snapshot']==0)/2
        delayed = sum(p['source_to_bitstream']['p50_us'] for p in probes if p['injected_completion_delay_us','snapshot']==5000)/2
        self_test = {'normal_p50_us':normal,'delayed_p50_us':delayed,'passed':delayed-normal>3000,'probes':probes}
        print(json.dumps({'self_test':self_test}),flush=True)
    hashed_sources = [*sources,*cpp,rtx/'internal.hpp',encode/'adapter.h',encode/'internal.h',Path(__file__).with_name('main.cpp'),Path(__file__),rtx/'cuda/output.ptx',encode/'cuda/convert.ptx']
    report = {'scope':('synthetic source through production VKMS snapshot, RTX HDR and HEVC10 NVENC' if args.snapshot else 'synthetic immutable DMA-BUF through real RTX HDR and HEVC10 NVENC')+' to locked bitstream; excludes presentation acquisition, SDK scheduling, transport and client',
              'source_sha256':{str(p.relative_to(ROOT)):hashlib.sha256(p.read_bytes()).hexdigest() for p in hashed_sources},
              'gpu':subprocess.check_output(['nvidia-smi','--query-gpu=name,driver_version','--format=csv,noheader'],text=True).strip(),
              'results':results,'completion_delay_self_test':self_test}
    if args.compare:
        baseline = json.loads(args.compare.read_text())
        previous = [r for r in baseline['results'] if r['mode']=='current']
        current = [r for r in results if r['mode']=='current']
        fields = ['width','height','frames','pace_fps','peak_nits','debanding','injected_completion_delay_us','snapshot']
        if baseline['scope'] != report['scope'] or baseline['gpu'] != report['gpu'] or any(previous[0][f] != current[0][f] for f in fields):
            raise SystemExit('Comparison requires identical GPU, scope and settings')
        before = sum(r['source_to_bitstream']['p95_us'] for r in previous)/len(previous)
        after = sum(r['source_to_bitstream']['p95_us'] for r in current)/len(current)
        report['comparison'] = {'baseline_p95_us':before,'current_p95_us':after,
                                'change_percent':(after/before-1)*100,
                                'passed':after<=before*(1+args.max_regression_percent/100)}
        print(json.dumps({'comparison':report['comparison']}),flush=True)
    args.output.write_text(json.dumps(report,indent=2)+'\n')
    if report.get('comparison',{}).get('passed') is False:
        raise SystemExit('Latency exceeded the requested regression tolerance')
    if self_test is not None and not self_test['passed']:
        raise SystemExit('Harness failed to detect the injected completion delay')


if __name__ == '__main__':
    main()
