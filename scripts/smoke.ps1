# Copyright (C) 2026 Voximir.
# SPDX-License-Identifier: GPL-3.0-only

param([string] $Runtime = (Join-Path $PSScriptRoot '../runtime'))
$ErrorActionPreference = 'Stop'
$Runtime = (Resolve-Path -LiteralPath $Runtime).Path
$projectRoot = Split-Path -Parent $PSScriptRoot
$testRoot = Join-Path $projectRoot 'target/smoke'
New-Item -ItemType Directory -Force $testRoot | Out-Null
$metadataJson = & cargo metadata --manifest-path (Join-Path $projectRoot 'Cargo.toml') --no-deps --format-version 1 --offline
if ($LASTEXITCODE -ne 0) { throw 'Could not read project metadata' }
$metadata = $metadataJson | ConvertFrom-Json
$package = $metadata.packages | Where-Object { $_.id -in $metadata.workspace_default_members }
$binary = @($package.targets | Where-Object { 'bin' -in $_.kind })
if ($binary.Count -ne 1) { throw 'Expected one default binary target' }
$appName = $binary[0].name
$exeSuffix = if ([Environment]::OSVersion.Platform -eq [PlatformID]::Win32NT) { '.exe' } else { '' }
$executable = Join-Path $Runtime "$appName$exeSuffix"
$ffmpeg = Join-Path $Runtime "ffmpeg/ffmpeg$exeSuffix"
$ffprobe = Join-Path $Runtime "ffmpeg/ffprobe$exeSuffix"
$reportedVersion = & $executable --version
if ($LASTEXITCODE -ne 0 -or $reportedVersion -ne "$appName $($package.version)") { throw "Version differs from Cargo: $reportedVersion" }
function Invoke-Rife([string[]] $Arguments) {
    $lines = @(& $executable @Arguments 2>&1)
    if ($LASTEXITCODE -ne 0) { throw ($lines -join "`n") }
    return ,$lines
}
function Get-Hashes([string] $Path) {
    $lines = & $ffmpeg -v error -i $Path -map 0:v:0 -pix_fmt rgb24 -f framemd5 -
    if ($LASTEXITCODE -ne 0) { throw "Could not hash $Path" }
    return ,@($lines | Where-Object { -not $_.StartsWith('#') } | ForEach-Object { ($_ -split ',')[-1].Trim() })
}
$raw = Join-Path $testRoot 'source.rgb'
$stream = [IO.File]::Create($raw)
try {
    for ($frame = 0; $frame -lt 6; $frame++) {
        $rgb = [byte[]]::new(64 * 64 * 3)
        for ($y = 0; $y -lt 64; $y++) {
            for ($x = 0; $x -lt 64; $x++) {
                $offset = ($y * 64 + $x) * 3
                if ($frame -lt 3) { $rgb[$offset] = 240 } else { $rgb[$offset + 2] = 240 }
                $box = 5 + ($frame % 3) * 16
                if ($y -ge 20 -and $y -lt 36 -and $x -ge $box -and $x -lt ($box + 12)) {
                    $rgb[$offset] = 245; $rgb[$offset + 1] = 245; $rgb[$offset + 2] = 245
                }
            }
        }
        $stream.Write($rgb, 0, $rgb.Length)
    }
} finally { $stream.Dispose() }
$source = Join-Path $testRoot 'source.nut'
& $ffmpeg -v error -f rawvideo -pixel_format rgb24 -video_size 64x64 -framerate 30000/1001 -i $raw -f lavfi -i 'sine=frequency=440:sample_rate=48000:duration=0.2002' -map 0:v -map 1:a -c:v ffv1 -level 3 -c:a pcm_s16le -shortest -y $source
if ($LASTEXITCODE -ne 0) { throw 'Fixture generation failed' }
$original = Get-Hashes $source
foreach ($threshold in @('0.3', '0', '1')) {
    $output = Join-Path $testRoot "threshold-$threshold.nut"
    $lines = Invoke-Rife -Arguments @('-i', $source, '-m', 'rife-v4.26', '-t', $threshold, '--video-codec', 'ffv1', '-y', $output)
    if ($lines.Count -ne 2 -or "$($lines[0])" -notmatch '^ +.+ video$' -or "$($lines[1])" -notmatch '^    Finished interpolating 12 frames in (?:[0-9]+\.[0-9]{2}s|[0-9]+m [0-9]+s)$') { throw "Normal logging changed: $($lines -join ' | ')" }
    if (($lines -join "`n") -match 'Done:|~|\x1b') { throw 'Completed redirected output contains stale estimates, a banner, or ANSI escapes' }
    $hashes = Get-Hashes $output
    if ($hashes.Count -ne 12) { throw 'Incorrect output frame count' }
    for ($frame = 0; $frame -lt 6; $frame++) {
        if ($hashes[$frame * 2] -ne $original[$frame]) { throw 'Original frame changed' }
        if ($threshold -eq '0' -and $hashes[$frame * 2 + 1] -ne $original[$frame]) { throw 'Threshold zero did not repeat every frame' }
    }
    if ($threshold -eq '0.3') {
        if ($hashes[5] -ne $original[2]) { throw 'Cut was interpolated' }
        if ($hashes[7] -eq $original[3] -or $hashes[7] -eq $original[4]) { throw 'Interpolation did not resume after cut' }
    }
    if ($threshold -eq '1' -and $hashes[5] -eq $original[2]) { throw 'Threshold one unexpectedly detected a cut' }
    $info = & $ffprobe -v error -show_entries 'stream=codec_type,avg_frame_rate' -of json $output | ConvertFrom-Json
    if (($info.streams | Where-Object codec_type -eq video).avg_frame_rate -ne '60000/1001') { throw 'Fractional FPS changed' }
    if (-not ($info.streams | Where-Object codec_type -eq audio)) { throw 'Audio was lost' }
}
$verbose = Invoke-Rife -Arguments @('-i', $source, '-m', 'rife-v4.26', '-v', '--video-codec', 'ffv1', '-y', (Join-Path $testRoot 'verbose.nut'))
$verboseText = $verbose -join "`n"
if ($verboseText -notmatch '(?m)^ +Loading model `rife-v4.26`$' -or $verboseText -notmatch '(?m)^ +Scanning video frames for output timing$' -or $verboseText -match 'Inspecting' -or $verboseText.IndexOf('Scanning video frames for output timing') -ge $verboseText.IndexOf('Loading model `rife-v4.26`')) { throw 'Unexpected status before model loading' }
foreach ($label in @('Scanning', 'Loading', 'Source', 'Target', 'Device')) {
    if ($verboseText -notmatch "(?m)^ *$label +") { throw "Missing verbose field: $label" }
}
if ($verboseText -notmatch '(?m)^ +Source 64x64 @ 29\.97 fps\r?\n +Target 64x64 @ 59\.94 fps$' -or $verboseText -notmatch '(?m)^ *Encoding +finalizing' -or $verboseText -match '(?m)Done:|^ *Video |^ *Model |Scenes|…|\.\.\.|Extracting frames:|Encoding video:|\x1b|\n\r?\n') { throw 'Verbose output mismatch' }
$failedOutput = Join-Path $testRoot 'encoder-failure.mp4'
$failure = @(& $executable -i $source -m rife-v4.26 --video-codec rife_missing_encoder -y $failedOutput 2>&1)
if ($LASTEXITCODE -eq 0 -or ($failure -join "`n") -notmatch '(?m)^error:' -or ($failure -join "`n") -notmatch 'rife_missing_encoder' -or (Test-Path -LiteralPath $failedOutput)) { throw 'Encoder failure lost its diagnostics or committed an output' }
if (@(Get-ChildItem -LiteralPath $testRoot -Directory -Filter ".$appName-*").Count -ne 0) { throw 'Staging was not cleaned up' }
if (@(Get-ChildItem -LiteralPath $testRoot -File -Filter '*.lock').Count -ne 0) { throw 'Output locks were not cleaned up' }
foreach ($flag in @('-h', '--help')) {
    $helpText = (& $executable $flag) -join "`n"
    if ($LASTEXITCODE -ne 0 -or $helpText -match 'Examples:' -or $helpText -notmatch ('Usage: ' + [regex]::Escape($appName))) { throw 'Help output mismatch' }
}
Write-Output 'Passed: cuts, resumed interpolation, endpoint thresholds, original pixels, doubled fractional FPS, audio, logging, help, encoder diagnostics and staging cleanup.'

# FPS scaling and packet clocks remain rational, independent of playback speed.
foreach ($case in @(@('4',24,'120000/1001'), @('0.5',3,'15000/1001'), @('1',6,'30000/1001'), @('1.5',9,'45000/1001'))) {
    $scale = $case[0]; $count = $case[1]; $rate = $case[2]
    $output = Join-Path $testRoot "scale-$scale.nut"
    $null = Invoke-Rife -Arguments @('-i',$source,'-f',"$scale",'--video-codec','ffv1','-y',$output)
    if ((Get-Hashes $output).Count -ne $count) { throw "Wrong count for FPS scale $scale" }
    $info = & $ffprobe -v error -select_streams v:0 -show_packets -show_entries stream=time_base:packet=pts,duration -of json $output | ConvertFrom-Json
    $tb = $info.streams[0].time_base -split '/'; $fps = $rate -split '/'
    foreach ($packet in $info.packets) {
        if ([long]$packet.duration * [long]$tb[0] * [long]$fps[0] -ne [long]$tb[1] * [long]$fps[1]) { throw "Wrong packet timing for FPS scale $scale" }
    }
}
# Slow motion generates quarters, keeps 2x FPS, and holds a cut until its timestamp.
$slow = Join-Path $testRoot 'slow.nut'
$null = Invoke-Rife -Arguments @('-i',$source,'-s','0.5','--video-codec','ffv1','-y',$slow)
$hashes = Get-Hashes $slow
if ($hashes.Count -ne 24) { throw 'Quarter-step slow motion did not generate 4N frames' }
foreach ($index in @(8,9,10,11)) {
    if ($hashes[$index] -ne $original[2]) { throw 'Slow-motion cut occurred before the next original timestamp' }
}
$info = & $ffprobe -v error -select_streams v:0 -show_entries stream=avg_frame_rate -of json $slow | ConvertFrom-Json
if ($info.streams[0].avg_frame_rate -ne '60000/1001') { throw 'Slow motion did not keep 2x FPS' }
foreach ($arguments in @(@('-f','0'), @('-s','0'), @('--slow','2'), @('--num-frame','24'), @('--time-step','0.5'), @('--tta'), @('--temporal-tta'), @('--uhd'), @('-s','0.5','--audio-codec','copy'))) {
    $null = & $executable -i $source @arguments -y (Join-Path $testRoot 'invalid.nut') 2>&1
    if ($LASTEXITCODE -eq 0) { throw "Accepted unsupported arguments: $arguments" }
}
# The large model is also loaded dynamically, with its exact directory name.
$null = Invoke-Rife -Arguments @('-i',$source,'-m','rife-v4.26-large','-f','4','--video-codec','ffv1','-y',(Join-Path $testRoot 'large.nut'))
Write-Output 'Passed: arbitrary counts, 4x rational FPS, quarter-step slow motion, cut timestamps, independent FPS/speed, removed features, audio-copy rejection, and both models.'
# A one-second fixture makes tempo and pitch checks independent of codec padding.
$audioSource = Join-Path $testRoot 'audio-source.nut'
& $ffmpeg -v error -f rawvideo -pixel_format rgb24 -video_size 64x64 -framerate 6 -i $raw -f lavfi -i 'sine=frequency=440:sample_rate=48000:duration=1' -map 0:v -map 1:a -c:v ffv1 -c:a pcm_s16le -shortest -y $audioSource
if ($LASTEXITCODE -ne 0) { throw 'Audio fixture generation failed' }
$audioOutput = Join-Path $testRoot 'pitch.nut'
$null = Invoke-Rife -Arguments @('-i',$audioSource,'-s','0.5','--video-codec','ffv1','--audio-codec','pcm_s16le','-y',$audioOutput)
$pcm = Join-Path $testRoot 'pitch.pcm'
& $ffmpeg -v error -i $audioOutput -map 0:a:0 -ac 1 -ar 48000 -c:a pcm_s16le -f s16le -y $pcm
if ($LASTEXITCODE -ne 0) { throw 'Could not decode retimed audio' }
$audioBytes = [IO.File]::ReadAllBytes($pcm)
$duration = $audioBytes.Length / 2.0 / 48000
if ([Math]::Abs($duration - 2.0) -gt 0.01) { throw "Slow audio duration mismatch: $duration" }
$start = [int](0.2 * 48000); $end = [int](1.7 * 48000); $crossings = 0
for ($index = $start; $index -lt $end; $index++) {
    $a = [BitConverter]::ToInt16($audioBytes, ($index-1)*2)
    $b = [BitConverter]::ToInt16($audioBytes, $index*2)
    if ($a -lt 0 -and $b -ge 0) { $crossings++ }
}
$frequency = $crossings / (($end-$start) / 48000.0)
if ([Math]::Abs($frequency - 440) -gt 3) { throw "Slow motion changed pitch: $frequency Hz" }
Write-Output "Passed: 2x slowdown audio duration $duration seconds and pitch $frequency Hz (source 440 Hz)."

# Independent speed factors and the exact original-frame sampling examples.
foreach ($case in @(@('2','0.25',48),@('2','0.125',96),@('1','0.5',12),@('1','1',6),@('1','2',3))) {
    $scale = $case[0]; $speed = $case[1]; $expected = $case[2]
    $output = Join-Path $testRoot "scale-$scale-speed-$speed.nut"
    $null = Invoke-Rife -Arguments @('-i',$source,'--fps-scale',"$scale",'--speed',"$speed",'--video-codec','ffv1','-y',$output)
    $hashes = Get-Hashes $output
    if ($hashes.Count -ne $expected) { throw "Incorrect FPS/speed combination: $scale/$speed" }
    if ($scale -eq '1' -and $speed -in @('1','2')) {
        for ($index = 0; $index -lt $hashes.Count; $index++) {
            if ($hashes[$index] -ne $original[$index * [int]$speed]) { throw 'Original-frame sampling changed pixels or interpolated during integer frame skipping' }
        }
    }
}
$mp4 = Join-Path $testRoot 'count-24.mp4'
$null = Invoke-Rife -Arguments @('-i',$source,'-f','4','-y',$mp4)
$info = & $ffprobe -v error -count_frames -select_streams v:0 -show_entries stream=avg_frame_rate,nb_read_frames -of json $mp4 | ConvertFrom-Json
if ($info.streams[0].avg_frame_rate -ne '120000/1001' -or $info.streams[0].nb_read_frames -ne '24') { throw 'MP4 lost exact arbitrary-count FPS/count' }
Write-Output 'Passed: all requested independent FPS/speed combinations, unchanged original frames, integer frame skipping, and MP4 exact 4x rational FPS.'
# Stored frame count (6) differs from the exact normalized count (8).
$vfr = Join-Path $testRoot 'vfr.nut'
& $ffmpeg -v error -i $source -map 0:v:0 -vf 'setpts=PTS+if(gte(N\,3)\,2/(30000/1001*TB)\,0)' -fps_mode passthrough -enc_time_base:v 1001:30000 -c:v ffv1 -y $vfr
if ($LASTEXITCODE -ne 0) { throw 'VFR fixture generation failed' }
$vfrOut = Join-Path $testRoot 'vfr-out.mp4'
$null = Invoke-Rife -Arguments @('-i',$vfr,'-f','4','-y',$vfrOut)
$info = & $ffprobe -v error -count_frames -select_streams v:0 -show_entries stream=avg_frame_rate,nb_read_frames -of json $vfrOut | ConvertFrom-Json
if ($info.streams[0].avg_frame_rate -ne '120000/1001' -or $info.streams[0].nb_read_frames -ne '32') { throw 'Scheduling used stored frame count instead of normalized count' }
$missing = @(& $executable -i $source -m rife-v4.28-large -y (Join-Path $testRoot 'missing.nut') 2>&1)
if ($LASTEXITCODE -eq 0 -or ($missing -join "`n") -notmatch 'models[\\/]rife-v4\.28-large' -or ($missing -join "`n") -match 'Unknown model') { throw 'Dynamic model name was not resolved as a directory' }
if (@(Get-ChildItem -LiteralPath $testRoot -Directory -Filter ".$appName-*").Count -ne 0 -or @(Get-ChildItem -LiteralPath $testRoot -File -Filter '*.lock').Count -ne 0) { throw 'Final staging/lock cleanup failed' }
Write-Output 'Passed: verified VFR normalization count (6 stored -> 8 normalized), dynamic future-model lookup, and final output cleanup.'
# Faster playback must retain pitch as well as slower playback.
$fastAudio = Join-Path $testRoot 'pitch-fast.nut'
$null = Invoke-Rife -Arguments @('-i',$audioSource,'-f','1','-s','2','--video-codec','ffv1','--audio-codec','pcm_s16le','-y',$fastAudio)
$fastPcm = Join-Path $testRoot 'pitch-fast.pcm'
& $ffmpeg -v error -i $fastAudio -map 0:a:0 -ac 1 -ar 48000 -c:a pcm_s16le -f s16le -y $fastPcm
if ($LASTEXITCODE -ne 0) { throw 'Could not decode fast audio' }
$fastBytes = [IO.File]::ReadAllBytes($fastPcm)
$fastDuration = $fastBytes.Length / 2.0 / 48000
if ([Math]::Abs($fastDuration - 0.5) -gt 0.01) { throw "Fast audio duration mismatch: $fastDuration" }
$start = [int](0.1 * 48000); $end = [int](0.4 * 48000); $crossings = 0
for ($index = $start; $index -lt $end; $index++) {
    if ([BitConverter]::ToInt16($fastBytes,($index-1)*2) -lt 0 -and [BitConverter]::ToInt16($fastBytes,$index*2) -ge 0) { $crossings++ }
}
$fastFrequency = $crossings / (($end-$start) / 48000.0)
if ([Math]::Abs($fastFrequency - 440) -gt 5) { throw "Fast playback changed pitch: $fastFrequency Hz" }
Write-Output "Passed: 2x playback audio duration $fastDuration seconds and pitch $fastFrequency Hz (source 440 Hz)."
