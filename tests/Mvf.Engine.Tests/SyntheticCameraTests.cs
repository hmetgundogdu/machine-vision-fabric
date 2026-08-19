using System.Text.Json;
using MachineVisionFabric.Integrations.SyntheticCamera;
using Mvf.Abstractions;

namespace Mvf.Engine.Tests;

/// <summary>
/// Pins the synthetic belt camera's contract. It is a <b>fixture</b> as much as a module: it is the only
/// source in the repo that produces real pixels with a declared 2-D shape, so anything testing image
/// behaviour downstream leans on these guarantees holding.
/// </summary>
public sealed class SyntheticCameraTests
{
    [Fact]
    public async Task Frames_DeclareTheirTwoDimensionalShape()
    {
        var frame = await FirstFrameAsync(new { width = 64, height = 32, frameIntervalMs = 0, frameCount = 1 });

        var descriptor = frame.Descriptor;
        Assert.NotNull(descriptor);
        Assert.Equal(PayloadMediaType.Image, descriptor!.Value.MediaType);
        Assert.Equal(PayloadElementType.UInt8, descriptor.Value.ElementType);

        // Rows before columns: a consumer that reshapes the buffer has to agree with the producer about
        // which dimension is which, or it reads a transposed image and never notices.
        Assert.Equal([32L, 64L], descriptor.Value.Shape);
        Assert.Equal(64 * 32, frame.ContentLength);
    }

    [Fact]
    public async Task APartEntersTheFrame_CrossesIt_AndLeavesAnEmptyBelt()
    {
        // framesPerPart 8 with a quarter-length gap: the part sweeps across the first six frames and the
        // last two are bare belt.
        var config = new { width = 64, height = 32, frameIntervalMs = 0, frameCount = 8, framesPerPart = 8, gapRatio = 0.25, defectEveryNthPart = 0 };
        var frames = await ReadFramesAsync(config, 8);

        var entering = await ReadPixelsAsync(frames[0]);
        var crossing = await ReadPixelsAsync(frames[3]);
        var gap = await ReadPixelsAsync(frames[7]);

        // A part enters from off-screen rather than appearing whole — the boundary case an inspection
        // pipeline meets on every real belt.
        Assert.DoesNotContain(entering, p => p > 150);
        Assert.Contains(crossing, p => p > 150);   // mid-sweep the part is unmistakably brighter than the belt
        Assert.DoesNotContain(gap, p => p > 150);  // an empty belt is a state the pipeline must also see
    }

    [Fact]
    public async Task EveryNthPart_CarriesADefectDarkerThanTheBelt()
    {
        // defectEveryNthPart 1 marks every part; frame 3 is mid-sweep, where the whole part is in view.
        var config = new { width = 64, height = 32, frameIntervalMs = 0, frameCount = 8, framesPerPart = 8, gapRatio = 0.25, defectEveryNthPart = 1 };
        var pixels = await ReadPixelsAsync((await ReadFramesAsync(config, 8))[3]);

        // The defect sits inside the bright part and is darker than the belt around it — that contrast is
        // what makes it detectable at all.
        Assert.Contains(pixels, p => p > 150);
        Assert.Contains(pixels, p => p < 35);
    }

    [Fact]
    public async Task TheSceneIsDeterministic()
    {
        var config = new { width = 64, height = 32, frameIntervalMs = 0, frameCount = 3, seed = 7 };

        var first = await ReadPixelsAsync((await ReadFramesAsync(config, 3))[2]);
        var second = await ReadPixelsAsync((await ReadFramesAsync(config, 3))[2]);

        // Same seed, same frame index, same image — otherwise a test that asserts on pixels is a coin toss.
        Assert.Equal(first, second);
    }

    private static async Task<IFrameEnvelope> FirstFrameAsync(object config) =>
        (await ReadFramesAsync(config, 1))[0];

    private static async Task<IReadOnlyList<IFrameEnvelope>> ReadFramesAsync(object config, int count)
    {
        var module = new SyntheticCameraModule();
        var element = JsonSerializer.SerializeToElement(config);
        await using var session = module.OpenSession(element, packageRoot: ".");

        var frames = new List<IFrameEnvelope>();
        using var cts = new CancellationTokenSource(TimeSpan.FromSeconds(5));
        await foreach (var frame in session.ReadFramesAsync(cts.Token))
        {
            frames.Add(frame);
            if (frames.Count >= count)
            {
                break;
            }
        }

        Assert.Equal(count, frames.Count);
        return frames;
    }

    private static async Task<byte[]> ReadPixelsAsync(IFrameEnvelope frame)
    {
        await using var stream = await frame.OpenReadAsync(CancellationToken.None);
        using var buffer = new MemoryStream();
        await stream.CopyToAsync(buffer);
        return buffer.ToArray();
    }
}
