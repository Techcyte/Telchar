import argparse
import json
import subprocess
import tempfile
from pathlib import Path
parser=argparse.ArgumentParser()
parser.add_argument('--qemu',required=True)
parser.add_argument('--proot',required=True)
parser.add_argument('--cc',required=True)
parser.add_argument('--make',required=True)
parser.add_argument('--cmake',required=True)
args=parser.parse_args()
cmake=Path(args.cmake)
assert (cmake/'bin/cmake').is_file()
assert (cmake/'bin/cmake').read_bytes()[18:20]==bytes([183,0])
def execute(tool,*argv):
    result=subprocess.run([args.proot,'-q',args.qemu,str(cmake/'bin'/tool),*argv],capture_output=True,text=True,timeout=90)
    if result.returncode:
        print(result.stdout,flush=True)
        print(result.stderr,flush=True)
        result.check_returncode()
    return result
with tempfile.TemporaryDirectory(prefix='telchar-cmake-smoke-') as directory:
    work=Path(directory)
    (work/'CMakeLists.txt').write_text('cmake_minimum_required(VERSION 3.20)\nproject(microvm_smoke C)\nenable_testing()\nadd_executable(smoke main.c)\nadd_test(NAME usable-result COMMAND smoke)\n')
    (work/'main.c').write_text('#include <stdio.h>\nint main(void) { puts("telchar-cmake-smoke"); return 0; }\n')
    version=execute('cmake','--version')
    assert 'cmake version 4.1.6' in version.stdout
    configured=execute('cmake','-S',str(work),'-B',str(work/'build'),'-G','Unix Makefiles','-DCMAKE_C_COMPILER='+args.cc,'-DCMAKE_MAKE_PROGRAM='+args.make)
    built=execute('cmake','--build',str(work/'build'))
    tested=execute('ctest','--test-dir',str(work/'build'),'--output-on-failure')
    assert '100% tests passed' in tested.stdout
    ran=subprocess.run([str(work/'build/smoke')],capture_output=True,text=True,check=True)
    assert ran.stdout=='telchar-cmake-smoke\n'
    print(json.dumps({'outcome':'usable','cmake_architecture':'aarch64','execution':'proot-qemu-user','smoke_compiler_architecture':'host','version':version.stdout,'configure':configured.stdout,'build':built.stdout,'ctest':tested.stdout,'program_stdout':ran.stdout},indent=2))
