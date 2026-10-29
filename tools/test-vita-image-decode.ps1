[CmdletBinding()]
param(
    [string]$Distro = 'pocket-ubuntu',
    [string]$Python = 'python'
)
$ErrorActionPreference = 'Stop'
$projectRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$buildRoot = Join-Path $projectRoot '.work/vita/image-decode'
$fixtures = Join-Path $buildRoot 'fixtures'

& $Python (Join-Path $PSScriptRoot 'vita-image-fixtures.py') $fixtures
if ($LASTEXITCODE -ne 0) { throw 'Original fixture generation failed' }
$projectPosix = (& wsl.exe -d $Distro -- wslpath -u $projectRoot.Replace('\', '/')).Trim()
if ($LASTEXITCODE -ne 0 -or !$projectPosix.StartsWith('/')) { throw 'WSL path resolution failed' }
& wsl.exe -d $Distro -- bash "$projectPosix/tools/test-vita-image-decode.sh"
if ($LASTEXITCODE -ne 0) { throw 'Native image verification failed; inspect doc/logs/vita/image-decode-*.log' }

$trackedFiles = @(
    'knife/vita/image_decode.c', 'knife/vita/image_decode.h',
    'knife/vita/tests/image_decode_test.c', 'knife/vita/tests/image_decode_allocation_test.c',
    'tools/vita-image-fixtures.py', 'tools/test-vita-image-decode.sh',
    'tools/test-vita-image-decode.ps1', '.work/vita/image-decode/fixtures/manifest.json',
    '.work/vita/image-decode/image_decode.o', '.work/vita/image-decode/link-probe.elf',
    'doc/logs/vita/image-decode-host.log', 'doc/logs/vita/image-decode-arm.log'
)
$files = foreach ($relative in $trackedFiles) {
    $path = Join-Path $projectRoot $relative
    [ordered]@{
        path = $relative
        bytes = (Get-Item -LiteralPath $path).Length
        sha256 = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant()
    }
}
$receipt = [ordered]@{
    schema = 'knife-vita-image-decode-build-v1'
    created_utc = [DateTime]::UtcNow.ToString('o')
    result = 'pass-host-sanitizers-and-vita-static-link'
    command = './tools/test-vita-image-decode.ps1'
    distro = $Distro
    public_api = [ordered]@{
        decode = 'int32_t kv_image_decode(const uint8_t *encoded, uint32_t length, KvDecodedImage *out)'
        release = 'void kv_image_free(KvDecodedImage *image)'
        vita_structure = 'uint32_t width,height,stride,length; uint8_t *pixels; sizeof=20, align=4, pixels_offset=16'
        pixel_format = 'RGBA8, tightly packed, straight alpha, complete image'
        ownership = 'Borrow input. Zero-init output; failure leaves it empty. Reject an owned output unchanged. Release with kv_image_free.'
        input_limit_bytes = 33554432
        width_limit = 960
        height_limit = 1080
        maximum_output_bytes = 4147200
        tracked_working_budget_bytes = 4456448
        jpeg_bootstrap_reserve_bytes = 65536
        libraries = @('png', 'jpeg', 'z', 'm')
    }
    host = [ordered]@{
        compiler = 'GCC 13.3.0'
        libpng_header_and_runtime = '1.6.43'
        libjpeg_turbo = '2.1.5'
        jpeg_abi = 80
        sanitizers = @('AddressSanitizer', 'UndefinedBehaviorSanitizer', 'LeakSanitizer')
        validation = @(
            'Transparent RGB channels preserved even at alpha=0; Adam7 exact pixels',
            'Palette+tRNS, gray+alpha and gray16 transforms',
            'Baseline, progressive, grayscale and full-size JPEG color/stride/opaque alpha',
            'Complete 960x1080 PNG and baseline JPEG output',
            'Every truncated prefix of PNG, baseline JPEG and progressive JPEG',
            'Bad IEND CRC, excessive dimensions, oversized input and unsupported CMYK',
            'Maximum-size progressive JPEG rejected by budget',
            'Zero/repeated frees and reject existing output ownership',
            'Source allocation failure injection: PNG9, Adam79, JPEG35, progressive43 allocation points'
        )
    }
    vita = [ordered]@{
        compiler = 'arm-vita-eabi-gcc 15.2.0'
        libpng = '1.6.58'
        libjpeg_turbo = '3.2.0'
        jpeg_abi = 62
        flags = @('-std=c11', '-Wall', '-Wextra', '-Werror', '-O2')
        artifact = 'ELF32 ARM little endian EABI5 hard float; NEON'
        compile_passed = $true
        static_library_link_passed = $true
        execution_verified = $false
    }
    limitations = @(
        'No real Vita or emulator execution is claimed by this receipt.',
        'Host and Vita codec library versions/ABI differ; only ARM compile and static link used the Vita libraries.',
        'Working budget counts output, contexts, custom allocator payload/header/alignment and a conservative 64KiB original JPEG bootstrap reserve; allocator bookkeeping, caller input and process/stack/RSS are outside this number.',
        'The original JPEG bootstrap has not been measured on Vita; the reserved 64KiB is conservative, not a measured Vita peak.',
        'Large progressive JPEG coefficient arrays may exceed the budget even when dimensions are permitted; files are not transcoded.',
        'PNG gamma/ICC transformations and JPEG CMYK/YCCK are not implemented; effects/rendering are separate.'
    )
    files = @($files)
}
$receiptPath = Join-Path $projectRoot 'doc/vita-image-decode-build.json'
$receipt | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath $receiptPath -Encoding utf8
Write-Output "Verified native decoder; receipt: $receiptPath"
