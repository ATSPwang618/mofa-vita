"""Worker-local RGBA8 acceleration for the unchanged 70% archive recipe."""
from concurrent.futures import ProcessPoolExecutor
from .production_driver import activate

ORIGINAL_RESIZE = None
MIN_PIXELS = 1024 * 1024
MAX_PIXELS = 24 * 1024 * 1024


def install():
    global ORIGINAL_RESIZE
    from .cuda_backend import activate as activate_cuda
    activate_cuda()
    from PIL import Image
    if ORIGINAL_RESIZE is None:
        ORIGINAL_RESIZE = Image.Image.resize
        Image.Image.resize = resize


def resize(image, size, resample=None, box=None, reducing_gap=None):
    from PIL import Image
    if (image.mode == 'RGBA' and resample == Image.Resampling.LANCZOS
            and box is None and reducing_gap is None and tuple(size) != image.size
            and MIN_PIXELS <= image.width * image.height <= MAX_PIXELS):
        import numpy as np
        from numba.cuda.cudadrv.driver import CudaAPIError
        from .cuda_kernels import resize_rgba8
        try:
            return Image.fromarray(resize_rgba8(np.asarray(image), tuple(size)))
        except (CudaAPIError, MemoryError) as error:
            print('70% CUDA allocation retry on CPU:', str(error), flush=True)
    return ORIGINAL_RESIZE(image, size, resample, box, reducing_gap)


def init_worker(root, base, volumes, deps):
    activate()
    import resize_story_variants as original
    original.init_worker(root, base, volumes, deps)
    install()


def pool(*args, **kwargs):
    kwargs['initializer'] = init_worker
    return ProcessPoolExecutor(*args, **kwargs)


def build(config):
    activate()
    import replay_resize as original
    if config.get('image_backend') != 'cuda':
        return original.build(config)
    # Reuse the original archive traversal, protection decisions, assembly and
    # complete-file SHA gates; only substitute the worker initialization.
    previous = original.ProcessPoolExecutor
    original.ProcessPoolExecutor = pool
    try:
        return original.build(config)
    finally:
        original.ProcessPoolExecutor = previous
