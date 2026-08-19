using System.Runtime.CompilerServices;
using Mvf.Abstractions;

namespace MachineVisionFabric.Integrations.SyntheticCamera;

/// <summary>
/// Draws the scene: a lit conveyor belt with parts moving across it, some of them defective.
///
/// <para>The scene is deliberately simple and <b>deterministic</b> — part position is a function of the
/// frame index, and the noise comes from a seeded generator — so a test can assert on what a given frame
/// contains, and two runs of a demo look the same. It is a stand-in for a camera, not a renderer.</para>
/// </summary>
internal sealed class SyntheticCameraSession(SyntheticCameraOptions options) : IFrameSourceSession
{
    // Grey levels, chosen so the three regions are unambiguous to a classifier and to the eye: belt is
    // clearly dark, part is clearly bright, defect is darker than the belt it sits on.
    private const byte BeltLevel = 45;
    private const byte PartLevel = 205;
    private const byte DefectLevel = 25;
    private const int NoiseAmplitude = 6;

    private readonly int _width = Math.Max(8, options.Width);
    private readonly int _height = Math.Max(8, options.Height);
    private readonly Random _noise = new(options.Seed);

    public int DeclaredCameraCount => 1;

    public int? EstimatedFrameCount => options.FrameCount > 0 ? options.FrameCount : null;

    public async IAsyncEnumerable<IFrameEnvelope> ReadFramesAsync(
        [EnumeratorCancellation] CancellationToken cancellationToken)
    {
        var index = 0;
        while (!cancellationToken.IsCancellationRequested)
        {
            if (options.FrameCount > 0 && index >= options.FrameCount)
            {
                yield break;
            }

            if (options.FrameIntervalMs > 0 && index > 0)
            {
                await Task.Delay(options.FrameIntervalMs, cancellationToken).ConfigureAwait(false);
            }

            var pixels = Render(index);
            yield return new GrayscaleFrameEnvelope(
                cameraId: "belt-cam",
                sequenceNumber: index,
                fileName: $"belt-{index:D6}.gray",
                pixels: pixels,
                width: _width,
                height: _height,
                timestampUtc: DateTime.UtcNow);

            index++;
        }
    }

    /// <summary>Renders one frame. Pure function of the frame index (plus seeded noise).</summary>
    internal byte[] Render(int frameIndex)
    {
        var pixels = new byte[_width * _height];

        // Belt with a little sensor noise, so nothing downstream can accidentally depend on a flat image.
        for (var i = 0; i < pixels.Length; i++)
        {
            pixels[i] = Clamp(BeltLevel + _noise.Next(-NoiseAmplitude, NoiseAmplitude + 1));
        }

        var framesPerPart = Math.Max(2, options.FramesPerPart);
        var partIndex = frameIndex / framesPerPart;
        var phase = (double)(frameIndex % framesPerPart) / framesPerPart;

        // The gap is the part of the cycle with an empty belt — an inspection pipeline has to cope with
        // "nothing in frame", so the simulator produces it.
        var gap = Math.Clamp(options.GapRatio, 0, 0.9);
        if (phase >= 1 - gap)
        {
            return pixels;
        }

        var travel = phase / (1 - gap); // 0..1 across the visible sweep
        var partWidth = _width / 4;
        var partHeight = _height / 2;
        var left = (int)((travel * (_width + partWidth)) - partWidth);
        var top = (_height - partHeight) / 2;

        FillRect(pixels, left, top, partWidth, partHeight, PartLevel);

        var defectEvery = options.DefectEveryNthPart;
        if (defectEvery > 0 && partIndex % defectEvery == defectEvery - 1)
        {
            // A dark blot roughly in the middle of the part — what a defect classifier is meant to catch.
            var defectSize = Math.Max(2, partWidth / 4);
            FillRect(
                pixels,
                left + ((partWidth - defectSize) / 2),
                top + ((partHeight - defectSize) / 2),
                defectSize,
                defectSize,
                DefectLevel);
        }

        return pixels;
    }

    private void FillRect(byte[] pixels, int left, int top, int width, int height, byte level)
    {
        var x0 = Math.Max(0, left);
        var y0 = Math.Max(0, top);
        var x1 = Math.Min(_width, left + width);
        var y1 = Math.Min(_height, top + height);

        for (var y = y0; y < y1; y++)
        {
            var row = y * _width;
            for (var x = x0; x < x1; x++)
            {
                pixels[row + x] = Clamp(level + _noise.Next(-NoiseAmplitude, NoiseAmplitude + 1));
            }
        }
    }

    private static byte Clamp(int value) => (byte)Math.Clamp(value, 0, 255);

    public ValueTask DisposeAsync() => ValueTask.CompletedTask;
}
