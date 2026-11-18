"""Optional bounded CUDA acceleration, preserving the locked production codec."""
import os
from pathlib import Path
import sys

ROOT=Path(__file__).resolve().parent
DEPS=ROOT/'.runtime/cuda-experiment'
DLL_HANDLES=[]
MIN_PIXELS=65536
MAX_PIXELS=24*1024*1024
CPU={}
ENABLED=False
PACKAGES={'numba':'0.68.1','numba-cuda':'0.30.4','numpy':'2.3.3',
          'llvmlite':'0.50.0','cuda-bindings':'12.9.10','cuda-core':'1.3.0',
          'cuda-pathfinder':'1.9.0','cuda-toolkit':'12.9.2.0',
          'nvidia-cuda-cccl-cu12':'12.9.27','nvidia-cuda-nvcc-cu12':'12.9.86',
          'nvidia-cuda-nvrtc-cu12':'12.9.86','nvidia-cuda-runtime-cu12':'12.9.79',
          'nvidia-nvjitlink-cu12':'12.9.86','packaging':'26.3'}


def setup():
    """Explicit installation only; normal CPU builds never install CUDA."""
    import subprocess
    subprocess.run([sys.executable,'-m','pip','install','--only-binary=:all:',
                    '--target',str(DEPS),*[f'{p}=={v}' for p,v in PACKAGES.items()]],check=True)


def activate():
    if not DEPS.is_dir():raise ValueError('CUDA cache missing; run --setup-cuda first')
    import importlib.metadata
    import site
    sys.path.insert(0,str(DEPS))
    site.addsitedir(str(DEPS))
    for name,expected in PACKAGES.items():
        if importlib.metadata.version(name)!=expected:
            raise ValueError('Unexpected CUDA dependency '+name+'; rerun --setup-cuda')
    os.environ.setdefault('NUMBA_CUDA_MAX_PENDING_DEALLOCS_COUNT','1')
    os.environ.setdefault('NUMBA_CUDA_MAX_PENDING_DEALLOCS_RATIO','0.02')
    if os.name=='nt' and not DLL_HANDLES:
        import ctypes
        for p in sorted({p.parent for p in DEPS.rglob('*.dll')}):
            DLL_HANDLES.append(os.add_dll_directory(str(p)))
        for pattern in ('cudart64_*.dll','nvJitLink_*.dll','nvrtc-builtins64_*.dll','nvrtc64_*.dll','nvvm64_*.dll'):
            for p in DEPS.rglob(pattern):DLL_HANDLES.append(ctypes.WinDLL(str(p)))
    from numba import cuda
    if not cuda.is_available():raise ValueError('CUDA device unavailable')
    return cuda


def install():
    global ENABLED
    import replay_images as images
    import bc3_production
    activate()
    from . import cuda_kernels
    if not CPU:
        CPU.update(resize=images.b.original.resize,grade=images.b.apply_lut,decode=bc3_production.decode,
                   variant=images.variant_job)
    images.b.original.resize=resize
    images.b.apply_lut=grade
    bc3_production.decode=decode
    images.variant_job=variant_job
    ENABLED=True


def eligible(w,h):return ENABLED and MIN_PIXELS<=w*h<=MAX_PIXELS


def resize(image,size):
    if image.size==size or not eligible(*image.size):return CPU['resize'](image,size)
    import numpy as np
    from PIL import Image
    from .cuda_kernels import resize_array
    return Image.fromarray(resize_array(np.asarray(image),size,image.getextrema()[3]==(255,255)))


def grade(image,key,luts):
    if not eligible(*image.size):return CPU['grade'](image,key,luts)
    import numpy as np
    from PIL import Image
    from .cuda_kernels import grade_array
    grey,lut=luts[(key['grade'],int(key['bright_bits'],16),int(key['contrast_bits'],16))]
    return Image.fromarray(grade_array(np.asarray(image),lut,grey))


def decode(data,w,h):
    if not eligible(w,h):return CPU['decode'](data,w,h)
    from PIL import Image
    from .cuda_kernels import bc3_decode
    return Image.fromarray(bc3_decode(data,w,h))


def variant_job(task):
    """Every output still passes the original bank SHA contract before publication."""
    global ENABLED
    import replay_images as images
    from numba.cuda.cudadrv.driver import CudaAPIError
    try:return CPU['variant'](task)
    except (ValueError,CudaAPIError,MemoryError) as error:
        # Retry numerical mismatches or a bounded GPU allocation on the CPU.
        # Input/corrupt-cache/other errors must remain fatal.
        if isinstance(error,ValueError) and not str(error).startswith('Output identity mismatch '):raise
        base,key=task
        print('CUDA CPU retry:',base,key['index'],str(error),flush=True)
        was_enabled=ENABLED
        ENABLED=False
        images.prepared_cache.clear()
        try:return CPU['variant'](task)
        finally:ENABLED=was_enabled


def describe():
    cuda=activate()
    device=cuda.get_current_device()
    name=device.name.decode() if isinstance(device.name,bytes) else str(device.name)
    return dict(backend='numba-cuda',device=name,packages=PACKAGES,
                accelerated=['associated-alpha Lanczos resize','LUT grading','production BC3 decode',
                             '70% RGBA8 Lanczos resize (large images)'],
                dxt_encoder='locked ISPC 1.0.1 (CPU)',small_images='CPU',
                min_pixels=MIN_PIXELS,max_pixels=MAX_PIXELS)
