param(
    [string]$Compiler = $env:KNIFE_VITA_CGC,
    [string]$VerifyWslDistribution = ''
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# Compile only the authored Cg source with the user's existing local tool.
# Neither the compiler nor any Sony module is copied into the project.
if ([string]::IsNullOrWhiteSpace($Compiler)) {
    $Compiler = 'D:/unity/PSVITA/sdk/host_tools/bin/psp2cgc.exe'
}
$compilerPath = (Get-Item -LiteralPath $Compiler).FullName
if (-not [IO.File]::Exists($compilerPath)) { throw 'Compiler must be a regular file' }
$root = (Get-Item -LiteralPath $PSScriptRoot).Parent.FullName
$source = Join-Path $root 'knife/vita/shaders/effects.cg'
$wgsl = Join-Path $root 'knife/knife-render/src/shader.wgsl'
$outputRoot = Join-Path $root '.work/vita/effects'
$headerPath = Join-Path $root 'knife/vita/shaders/generated/effects.h'
$receiptPath = Join-Path $root 'doc/vita-effects-shaders-build.json'
foreach ($directory in @($outputRoot, [IO.Path]::GetDirectoryName($headerPath))) {
    [void][IO.Directory]::CreateDirectory($directory)
}
$utf8 = [Text.UTF8Encoding]::new($false)
$logPath = Join-Path $outputRoot 'build.log'
[IO.File]::WriteAllText($logPath, '', $utf8)

function Digest([string]$Path) {
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}
function Relative([string]$Path) {
    if (-not $Path.StartsWith($root + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Output is outside workspace: $Path"
    }
    return $Path.Substring($root.Length + 1).Replace('\', '/')
}
function Log([string]$Message) {
    [IO.File]::AppendAllText($logPath, $Message + "`n", $utf8)
    Write-Host $Message
}
function Compile([string[]]$Arguments) {
    Log ('psp2cgc ' + ($Arguments -join ' '))
    $lines = @(& $compilerPath @Arguments 2>&1)
    $exit = $LASTEXITCODE
    foreach ($line in $lines) { Log ([string]$line) }
    if ($exit -ne 0) { throw "Shader compilation failed: exit $exit" }
}
function Range([byte[]]$Bytes, [long]$Offset, [long]$Length, [string]$Label) {
    if ($Offset -lt 0 -or $Length -lt 0 -or $Offset + $Length -gt $Bytes.Length) {
        throw "GXP $Label exceeds artifact: offset=$Offset length=$Length"
    }
}
function U32([byte[]]$Bytes, [int]$Offset) {
    Range $Bytes $Offset 4 'u32'
    return [BitConverter]::ToUInt32($Bytes, $Offset)
}
function I32([byte[]]$Bytes, [int]$Offset) {
    Range $Bytes $Offset 4 'i32'
    return [BitConverter]::ToInt32($Bytes, $Offset)
}
function Reflect-Gxp([string]$Path, [bool]$Fragment) {
    $b = [IO.File]::ReadAllBytes($Path)
    if ($b.Length -lt 156 -or $b.Length -gt 1048576 -or $b.Length % 4 -ne 0) {
        throw "Invalid GXP size/alignment: $Path ($($b.Length))"
    }
    if ($b[0] -ne 0x47 -or $b[1] -ne 0x58 -or $b[2] -ne 0x50 -or $b[3] -ne 0) {
        throw "Invalid GXP magic: $Path"
    }
    $programSize = U32 $b 8
    if ($programSize -gt $b.Length -or $programSize -le 156 -or $b.Length - $programSize -gt 3) {
        throw "Invalid GXP program size: $Path"
    }
    for ($p = $programSize; $p -lt $b.Length; $p++) {
        if ($b[$p] -ne 0) { throw "Nonzero GXP end padding: $Path" }
    }
    $programFlags = U32 $b 20
    if ((($programFlags -band 1) -eq 1) -ne $Fragment) { throw "GXP stage mismatch: $Path" }
    $instructionCount = U32 $b 60
    $instructionOffset = 64 + (I32 $b 64)
    if ($instructionCount -le 0) { throw "GXP has no primary instructions: $Path" }
    Range $b $instructionOffset (8L * $instructionCount) 'primary instructions'
    $parameterCount = U32 $b 36
    $table = 40 + (I32 $b 40)
    Range $b $table (16L * $parameterCount) 'parameter table'
    $varyings = 44 + (I32 $b 44)
    Range $b $varyings 32 'varyings'
    $uniformWords = U32 $b 100
    $parameters = [Collections.Generic.List[object]]::new()
    $names = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    $categoryNames = @('ATTRIBUTE', 'UNIFORM', 'SAMPLER', 'AUXILIARY_SURFACE', 'UNIFORM_BUFFER')
    $typeNames = @('F32', 'F16', 'C10', 'U32', 'S32', 'U16', 'S16', 'U8', 'S8', 'AGGREGATE')
    for ($p = 0; $p -lt $parameterCount; $p++) {
        $offset = $table + 16 * $p
        $nameOffset = $offset + (I32 $b $offset)
        Range $b $nameOffset 1 'parameter name'
        $end = $nameOffset
        while ($end -lt $programSize -and $b[$end] -ne 0) { $end++ }
        if ($end -ge $programSize -or $end - $nameOffset -gt 256) { throw 'Invalid GXP parameter name' }
        $name = [Text.Encoding]::ASCII.GetString($b, $nameOffset, $end - $nameOffset)
        if (-not $names.Add($name)) { throw "Duplicate GXP parameter: $name" }
        $category = $b[$offset + 4] -band 15
        $type = $b[$offset + 4] -shr 4
        $components = $b[$offset + 5] -band 15
        $container = $b[$offset + 5] -shr 4
        $arraySize = U32 $b ($offset + 8)
        $resource = U32 $b ($offset + 12)
        if ($category -gt 2 -or $type -ne 0 -or $arraySize -ne 1) {
            throw "Unexpected GXP parameter ABI: $name"
        }
        if ($category -eq 1 -and ($container -ne 14 -or $components -notin @(2,4) -or $resource + $components -gt $uniformWords)) {
            throw "Invalid default-buffer uniform: $name"
        }
        if ($category -eq 2) {
            $expectedUnit = @{ tex0 = 0; tex1 = 1; tex2 = 2; soft_lut = 3 }
            if (-not $expectedUnit.ContainsKey($name) -or $resource -ne $expectedUnit[$name]) {
                throw "Unexpected sampler unit: $name / $resource"
            }
        }
        $parameters.Add([ordered]@{
            name = $name
            table_offset_bytes = $offset
            name_offset_bytes = $nameOffset
            category = $categoryNames[$category]
            category_id = $category
            type = $typeNames[$type]
            type_id = $type
            component_count = $components
            container_index = $container
            semantic_id = $b[$offset + 6]
            semantic_index = $b[$offset + 7]
            array_size = $arraySize
            resource_index = $resource
        })
    }
    if ($Fragment) {
        if ($b[$varyings + 10] -ne 1 -or $b[$varyings + 11] -ne 4) {
            throw 'Unexpected PSP2 fragment COLOR output ABI (expected F16x4)'
        }
        $output = [ordered]@{ type = 'F16'; type_id = 1; component_count = 4 }
    } else {
        $attributeNames = @('v.pos', 'v.uv', 'v.color', 'v.hx', 'v.hy', 'v.s0', 'v.s1', 'v.s2')
        if ($parameters.Count -ne 8) { throw 'VS must retain all eight 112-byte vertex attributes' }
        for ($p = 0; $p -lt 8; $p++) {
            $attribute = $parameters[$p]
            if ($attribute.name -ne $attributeNames[$p] -or $attribute.category_id -ne 0 -or $attribute.component_count -ne 4 -or $attribute.resource_index -ne 4 * $p) {
                throw 'VS attribute reflection does not match expected stream register map'
            }
        }
        $output = $null
    }
    return [ordered]@{
        magic = '47585000'
        file_bytes = $b.Length
        program_bytes = $programSize
        trailing_zero_padding_bytes = $b.Length - $programSize
        version = "$($b[4]).$($b[5])"
        sdk_version_hex = ('0x{0:x4}' -f [BitConverter]::ToUInt16($b,6))
        program_flags_hex = ('0x{0:x8}' -f $programFlags)
        primary_instruction_count = $instructionCount
        primary_instruction_offset_bytes = $instructionOffset
        parameter_table_offset_bytes = $table
        parameter_count = $parameterCount
        default_uniform_buffer_f32_words = $uniformWords
        default_uniform_buffer_bytes = 4 * $uniformWords
        fragment_output = $output
        parameters = $parameters.ToArray()
    }
}

$referenceEntries = @('vs_main', 'fs_sprite', 'fs_noalpha', 'fs_forcealpha', 'fs_fill', 'fs_text', 'fs_rim_text', 'fs_mono', 'fs_nega', 'fs_transition', 'fs_rule', 'fs_blur', 'fs_alter', 'fs_present', 'fs_present_srgb', 'fs_ps')
$vitaEntries = @('fs_group_composite', 'fs_sprite_mask')
$entries = $referenceEntries + $vitaEntries
$wgslEntries = @([regex]::Matches([IO.File]::ReadAllText($wgsl), '@(vertex|fragment)\s+fn\s+([a-zA-Z0-9_]+)') | ForEach-Object { $_.Groups[2].Value })
if ((($referenceEntries | Sort-Object) -join ',') -cne (($wgslEntries | Sort-Object) -join ',')) {
    throw 'Cg entry inventory must match every WGSL vertex/fragment entry'
}
$version = @(& $compilerPath -version 2>&1)
if ($LASTEXITCODE -ne 0) { throw 'Cannot query compiler version' }
Log ($version -join "`n")
$common = @('-O3', '-bestprecision', '-nofastmath', '-nofastint', '-W3', '-Werror', '-Wperf', '-Wsuppress=7302', '-nofx', '-nocolor', '-lang', 'en')
$records = [Collections.Generic.List[object]]::new()
$header = [Text.StringBuilder]::new()
[void]$header.Append("/* Generated by tools/compile-vita-effects.ps1; do not edit. */`n")
[void]$header.Append("/* Complete WGSL inventory plus explicit Vita-only group composite; offline validation only. */`n")
[void]$header.Append("/* Bind uQuadHx/uQuadHy PER QUAD, packed uniforms as F32. */`n")
[void]$header.Append("/* See effects.cg and doc/vita-effects-shaders-build.json for ABI/contracts. */`n")
[void]$header.Append("#ifndef KNIFE_VITA_GENERATED_EFFECTS_H`n#define KNIFE_VITA_GENERATED_EFFECTS_H`n`n")
[void]$header.Append("#if defined(__cplusplus)`n#define KNIFE_EFFECTS_ALIGN alignas(4)`n#else`n#define KNIFE_EFFECTS_ALIGN _Alignas(4)`n#endif`n`n")
foreach ($entry in $entries) {
    $isFragment = $entry.StartsWith('fs_')
    $profile = if ($isFragment) { 'sce_fp_psp2' } else { 'sce_vp_psp2' }
    $arguments = @('-profile', $profile, '-entry', $entry) + $common
    if (-not $isFragment) { $arguments += '-DKNIFE_VERTEX_SHADER=1' }
    $output = Join-Path $outputRoot ($entry + '.gxp')
    $repeat = Join-Path $outputRoot ($entry + '.repeat.gxp')
    Compile ($arguments + @('-o', $output, $source))
    $metadata = Reflect-Gxp $output $isFragment
    Compile ($arguments + @('-o', $repeat, $source))
    [void](Reflect-Gxp $repeat $isFragment)
    $digest = Digest $output
    $repeatDigest = Digest $repeat
    if ($digest -ne $repeatDigest) { throw "Repeated compile differs: $entry" }
    $b = [IO.File]::ReadAllBytes($output)
    $symbol = 'knife_effects_' + $entry + '_gxp'
    [void]$header.Append("/* $entry; SHA-256: $digest */`n")
    [void]$header.Append("KNIFE_EFFECTS_ALIGN static const unsigned char $symbol`[] = {`n")
    for ($start = 0; $start -lt $b.Length; $start += 12) {
        $end = [Math]::Min($start + 11, $b.Length - 1)
        $row = for ($index = $start; $index -le $end; $index++) { '0x{0:x2}' -f $b[$index] }
        [void]$header.Append('    ' + ($row -join ', ') + ",`n")
    }
    [void]$header.Append("};`nstatic const unsigned int $($symbol)_size = sizeof($symbol);`n`n")
    $records.Add([ordered]@{
        entry = $entry
        stage = if ($isFragment) { 'fragment' } else { 'vertex' }
        profile = $profile
        compiler_arguments = $arguments
        artifact = [ordered]@{ path = Relative $output; bytes = $b.Length; sha256 = $digest }
        repeated_artifact = [ordered]@{ path = Relative $repeat; sha256 = $repeatDigest }
        repeated_compile_identical = $true
        header_symbol = $symbol
        header_size_symbol = $symbol + '_size'
        metadata = $metadata
    })
    Log ("Validated $entry : $($b.Length) bytes, $($metadata.parameter_count) parameters; repeated SHA-256 identical")
}
[void]$header.Append("#undef KNIFE_EFFECTS_ALIGN`n#endif`n")
[IO.File]::WriteAllText($headerPath, $header.ToString(), $utf8)

# Verify generated array contents against the GXP bytes, not just source declarations.
$headerText = [IO.File]::ReadAllText($headerPath)
foreach ($record in $records) {
    $pattern = 'unsigned char ' + [regex]::Escape($record.header_symbol) + '\[\] = \{([^}]+)\};'
    $match = [regex]::Match($headerText, $pattern)
    if (-not $match.Success) { throw 'Missing generated GXP array' }
    $array = [byte[]]@([regex]::Matches($match.Groups[1].Value, '0x([0-9a-f]{2})') | ForEach-Object { [Convert]::ToByte($_.Groups[1].Value,16) })
    $sha = [Security.Cryptography.SHA256]::Create()
    try { $arrayDigest = [BitConverter]::ToString($sha.ComputeHash($array)).Replace('-', '').ToLowerInvariant() }
    finally { $sha.Dispose() }
    if ($array.Length -ne $record.artifact.bytes -or $arrayDigest -ne $record.artifact.sha256) { throw 'Generated GXP array differs from compiler artifact' }
}

# Optional, reproducible integration check with the installed host and Vita C
# compilers. The ARM object is compiled only; this does not execute Vita code.
$headerCompileChecks = [ordered]@{requested=$false;host_c11_compiled=$false;host_alignment_executed=$false;vita_arm_c11_compiled=$false}
if (-not [string]::IsNullOrWhiteSpace($VerifyWslDistribution)) {
    $checkPath = Join-Path $outputRoot 'header_check.c'
    $check = [Collections.Generic.List[string]]::new()
    $check.Add('#include <stdint.h>')
    $check.Add('#include "../../../knife/vita/shaders/generated/effects.h"')
    foreach ($record in $records) {
        $symbol = $record.header_symbol
        $check.Add('_Static_assert(__alignof__(' + $symbol + ') >= 4, "GXP alignment");')
        $check.Add('_Static_assert(sizeof(' + $symbol + ') == ' + $record.artifact.bytes + ', "GXP length");')
    }
    $check.Add('int main(void) {')
    foreach ($record in $records) {
        $symbol = $record.header_symbol
        $check.Add('    if (((uintptr_t)' + $symbol + ' & 3u) != 0u || ' + $symbol + '_size != sizeof(' + $symbol + ') || ' + $symbol + '[0] != 0x47u) return 1;')
    }
    $check.Add('    return 0;')
    $check.Add('}')
    [IO.File]::WriteAllText($checkPath, ($check -join "`n") + "`n", $utf8)
    $wslPathLines = @(& wsl.exe -d $VerifyWslDistribution --exec wslpath -a $checkPath 2>&1)
    if ($LASTEXITCODE -ne 0 -or $wslPathLines.Count -ne 1) { throw 'Cannot resolve the header-check source in WSL' }
    $wslCheckPath = [string]$wslPathLines[0]
    $headerCommands = @(
        @('gcc','-std=c11','-Wall','-Wextra','-Werror',$wslCheckPath,'-o',($wslCheckPath + '.host')),
        @(($wslCheckPath + '.host')),
        @('/opt/vitasdk/bin/arm-vita-eabi-gcc','-std=c11','-Wall','-Wextra','-Werror','-mcpu=cortex-a9','-c',$wslCheckPath,'-o',($wslCheckPath + '.arm.o'))
    )
    foreach ($command in $headerCommands) {
        Log ('WSL ' + $VerifyWslDistribution + ': ' + ($command -join ' '))
        $lines = @(& wsl.exe -d $VerifyWslDistribution --exec @command 2>&1)
        $commandExit = $LASTEXITCODE
        foreach ($line in $lines) { Log ([string]$line) }
        if ($commandExit -ne 0) { throw "Header integration check failed: exit $commandExit" }
    }
    $headerCompileChecks = [ordered]@{requested=$true;distribution=$VerifyWslDistribution;source=Relative $checkPath;source_sha256=Digest $checkPath;host_c11_compiled=$true;host_alignment_executed=$true;vita_arm_c11_compiled=$true;vita_runtime_executed=$false}
}

$vertex = @(
    @{name='v.pos';offset=0;components=2}, @{name='v.uv';offset=8;components=2},
    @{name='v.color';offset=16;components=4}, @{name='v.hx';offset=32;components=4},
    @{name='v.hy';offset=48;components=4}, @{name='v.s0';offset=64;components=4},
    @{name='v.s1';offset=80;components=4}, @{name='v.s2';offset=96;components=4}
)
$contracts = [ordered]@{
    vertex = [ordered]@{
        stride_bytes = 112
        attributes = $vertex
        format = 'F32'
        register_index_source = 'Use reflected resource_index, not byte offsets'
        widened_attribute_metadata = 'All GXP attributes report 4 components; stream counts remain 2 for pos/uv and 4 for others'
        position = 'Input is clip-space xy; output is (x,y,0,1)'
        varying_semantics = [ordered]@{uv='TEXCOORD0';color='TEXCOORD1';hx='TEXCOORD2';hy='TEXCOORD3';s0='TEXCOORD4';s1='TEXCOORD5';s2='TEXCOORD6'}
    }
    quad_binding = [ordered]@{
        frequency = 'PER QUAD; update fragment default uniform buffer before every draw with different hx/hy'
        source = 'Copy exact Vertex.haze[0] to uQuadHx and Vertex.haze[1] to uQuadHy'
        batch_constraint = 'All vertices in one quad have identical hx/hy; unequal quads must not share a draw'
        precision = 'Use sceGxmSetUniformDataF data as float32; never pack to half or interpolate packed values'
        shader_families = @(
            [ordered]@{entries=@('fs_sprite','fs_fill','fs_mono','fs_nega','fs_noalpha','fs_forcealpha','fs_rule','fs_ps');uQuadHx=@('horizontal amplitude in UV: finite signed float','horizontal waves per uv.y: finite signed float','horizontal phase in cycles: finite signed float','intermediate premultiply flag: 0 or 1, tested >0.5');uQuadHy=@('vertical amplitude in UV: finite signed float','vertical waves per uv.y: finite signed float','vertical phase in cycles: finite signed float','unused')},
            [ordered]@{entries=@('fs_text');uQuadHx=@('unused','unused','unused','unused');uQuadHy=@('unused','unused','bordered glyph: 0 or 1, tested >0.5','4x4 derivative box filter: 0 or 1, tested >0.5')},
            [ordered]@{entries=@('fs_rim_text');uQuadHx=@('u_min: finite normalized UV','v_min: finite normalized UV','u_max >= u_min: finite normalized UV','v_max >= v_min: finite normalized UV');uQuadHy=@('du per canvas pixel: finite float','dv per canvas pixel: finite float','rim opacity: 0..1','packed RGB24 integer: 0..16777215 inclusive')}
        )
    }
    uniforms = [ordered]@{
        uTex0Size = 'Mip-0 texel dimensions of tex0, float2, finite positive integers; fs_blur only'
        uTex1Size = 'Mip-0 texel dimensions of tex1 backdrop, float2, finite positive integers; fs_ps only'
        uSoftLutSize = 'Mip-0 texel dimensions of soft_lut, float2, finite positive integers; soft-mask sprite family'
        dimension_limit = 'All dimensions must be exactly representable in F32; platform texture limits apply independently'
        tu_params = [ordered]@{
            fs_transition = @('progress 0..1','packed rule word','use rule: 0 or 1','unused')
            fs_rule = @('progress 0..1','packed rule word','positive target width pixels','positive target height pixels')
            fs_blur = @('horizontal step pixels: finite signed float','vertical step pixels: finite signed float','sigma: finite float, shader lower bound 0.01','half tap count: finite i32-convertible float, normally ceil(3*sigma)')
            fs_ps = @('PsOp integer 1..24','fill colour selector: 0 or 1','positive target width pixels','positive target height pixels')
        }
        tu_params2 = [ordered]@{
            fs_blur = @('blur mode integer 0..2','unused','unused','unused')
            fs_ps = @('rule progress 0..1 or negative to disable','packed rule word, unused if disabled','noalpha source: 0 or 1','group mask: integer 0 or 1; requires disabled rule and noalpha=0')
        }
        per_entry_active_parameters = 'Use shaders[].metadata.parameters; optimizer omits unused uniforms, and packing differs between fragment programs. Query each by its exact reflected name.'
    }
    samplers = @(
        @{name='tex0';unit=0;role='source image/glyph/frame'},
        @{name='tex1';unit=1;role='second frame, pre-quad backdrop copy, or group mask in fs_group_composite'},
        @{name='tex2';unit=2;role='rule texture or group mask in fs_ps group mode'},
        @{name='soft_lut';unit=3;role='soft-mask lookup image'}
    )
    sampler_state = 'Ordinary sampling retains original filtering/addressing. Integer textureLoad uses mip-0 texel centres and explicit zero for OOB; bind nearest sampling for soft_lut to avoid interpolation roundoff at exact centres. All images are sampled UNORM unless the original specific pass requires sRGB decoding.'
    integer_semantics = [ordered]@{
        signed_casts = 'Cg int(value), int2(value) truncate toward zero as WGSL i32 conversions; -nofastint retains full precision. Examples: int(-1.9)=-1; floor(-1.9)=-2. Nonfinite and out-of-i32-range inputs are outside contract.'
        signed_cast_sites = @('soft LUT row','soft LUT floor(t) to k, then int(n)','blur half tap count','blur mode after +0.5','backdrop WPOS to int2')
        packed_unsigned = 'Nonnegative integral input domain makes floor equivalent to WGSL u32 truncation. Quotients/remainders by powers of two replace uint bit operations without bit loss within the declared ranges.'
        rule_word = [ordered]@{minimum_active=8;maximum=2047;highest_bit=10;mirror_x_bit=0;mirror_y_bit=1;invert_bit=2;soft_width_bits='3..10';soft_width_minimum=1;soft_width_maximum=255;producer='knife/knife-render/src/lib.rs:79 rule_flags'}
        rim_rgb = [ordered]@{minimum=0;maximum=16777215;highest_bit=23;layout='0xRRGGBB';producer='knife/knife/src/text.rs:277 rimmed_quad_edge'}
        precise_integer_limit = 16777216
        invalid_packed_input_policy = 'Backend must reject noninteger/negative/out-of-range packed words before conversion/upload; do not clamp, half-pack, or accept wider uints and silently lose bits. This shader does not recover bits already lost by CPU float conversion.'
        full_u32_domain_supported = $false
    }
    precision_and_colour = 'Source arithmetic and uniforms are float32 with -bestprecision/-nofastmath. PSP2 compiler emits F16x4 COLOR result metadata. Patcher output format, blend state, UNORM/sRGB target choice and hardware pixel error require runtime validation. Premultiplied formulas, original early exits, and lack of extra clamps are retained.'
    fragment_position = 'WPOS must correspond to WGSL target pixel centres with matching viewport orientation; integer backdrop fetch uses truncation. Validate y direction and half-pixel convention with a runtime fixture.'
    ps_operation_inventory = @(1..24)
    preserved_behaviour = @('complete VS with eight input attributes','all 15 fragment entries','24 PsOp branches including original zero-alpha exits','STRA haze x/y wave expressions','soft rect transform and LUT interpolation','glyph derivative 4x4 filter and bordered coverage','eight-tap RGB24 rim','three blur modes with unbounded uniform tap count and original normalization','rule mirror/invert/softness bits','alter/present/present_srgb transfer formulas')
}
Log ('Generated ' + (Relative $headerPath))
Log ('Receipt ' + (Relative $receiptPath))
$receipt = [ordered]@{
    built_at = [DateTime]::UtcNow.ToString('o')
    scope = 'Complete 16-entry WGSL-to-Cg translation plus one Vita-only group composite entry, local offline compilation and binary metadata checks; runtime equivalence is a separate gate'
    compiler = [ordered]@{path=$compilerPath.Replace('\','/');sha256=Digest $compilerPath;version=$version -join "`n";redistributed=$false}
    entrypoint = [ordered]@{path=Relative $PSCommandPath;sha256=Digest $PSCommandPath}
    source = [ordered]@{path=Relative $source;sha256=Digest $source}
    reference_wgsl = [ordered]@{path=Relative $wgsl;sha256=Digest $wgsl;entry_count=$wgslEntries.Count;entries=$wgslEntries}
    vita_only_entries = $vitaEntries
    group_composite_contract = 'Same-size RGBA8 pixel/mask, full-canvas UVs; quantize pixel.a*mask.a to UNORM8 before final opacity. No temporary RGBA8 target. Separate native readback comparison required before game integration.'
    compiler_compatibility = [ordered]@{
        unsupported_flat_qualifiers = 'Actual profile diagnostics D5496/D5405 for nointerpolation and flat; exact flat hx/hy replaced by per-quad uniforms, with 112B VS ABI retained'
        unsupported_uint = 'Actual profile diagnostic D5517 for uint; packed-word arithmetic uses exact float32 domain, signed int conversions retained'
        unsupported_switch = 'Actual profile diagnostic D4438 for switch; all 24 cases translated to if/else, including shared 11/12 branch'
        suppressed_warning = [ordered]@{id=7302;reason='Performance advice to narrow full-precision float/int conversion. Retain original 32-bit semantics instead of shortening to 16-bit; this one advisory is suppressed, all remaining W3/Wperf warnings remain errors.'}
    }
    contracts = $contracts
    shaders = $records.ToArray()
    header = [ordered]@{path=Relative $headerPath;bytes=(Get-Item -LiteralPath $headerPath).Length;sha256=Digest $headerPath;alignment_bytes=4;array_byte_roundtrip_verified=$true;compile_checks=$headerCompileChecks}
    build_log = [ordered]@{path=Relative $logPath;sha256=Digest $logPath}
    validation = [ordered]@{compiled_entry_count=$entries.Count;all_entries_compiled=$true;all_entries_compiled_twice=$true;reference_16_compiled_twice=$true;repeat_sha256_identical=$true;magic_size_padding_stage_instruction_bounds_verified=$true;all_parameters_reflected=$true;all_eight_vertex_attributes_retained=$true;sampler_units_verified=$true;uniform_f32_layout_verified=$true;generated_array_bytes_verified=$true;sdk_program_check_verified=$false;patcher_link_verified=$false;runtime_verified=$false;hardware_verified=$false;pixel_equivalence_verified=$false}
}
[IO.File]::WriteAllText($receiptPath, ($receipt | ConvertTo-Json -Depth 20) + "`n", $utf8)
