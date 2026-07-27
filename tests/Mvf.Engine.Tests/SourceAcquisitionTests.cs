using Mvf.Engine.Execution;
using Mvf.Graph.Execution;
using Mvf.Sdk;

namespace Mvf.Engine.Tests;

/// <summary>
/// Covers the source acquisition-timing plumbing: the base session must report wait and queue age for every
/// frame with no per-module work, and the receive time only for frames whose fetch was wrapped in
/// <c>BeginAcquire()</c>. The accumulator must roll those per-frame samples into the right averages, dividing
/// receive by the frames that reported it rather than by all frames.
/// </summary>
public sealed class SourceAcquisitionTests
{
    // A background source that publishes a fixed set of frames; a flag per frame says whether its "fetch" is
    // wrapped in BeginAcquire (with a real delay, so a receive time is measurable).
    private sealed class FakeBackgroundSource(bool[] measureReceive) : BackgroundFrameSourceSession(1, measureReceive.Length)
    {
        public FakeBackgroundSource Start()
        {
            StartBackgroundProducer(ProduceAsync);
            return this;
        }

        private async Task ProduceAsync(CancellationToken ct)
        {
            for (var i = 0; i < measureReceive.Length; i++)
            {
                var frame = FrameEnvelopeFactory.FromBytes("cam", i + 1, $"f{i}.bin", [1, 2, 3]);
                if (measureReceive[i])
                {
                    using (BeginAcquire())
                        await Task.Delay(8, ct);   // a fetch that takes time — this is the receive span
                }
                await PublishAsync(frame, ct);
            }
        }
    }

    [Fact]
    public async Task ReportsReceiveOnlyForFramesThatOptedIn()
    {
        await using var source = new FakeBackgroundSource([true, false, true]).Start();

        var samples = new List<FrameAcquisitionSample>();
        await foreach (var _ in source.ReadFramesAsync(CancellationToken.None))
        {
            var sample = source.GetLastAcquisition();
            Assert.NotNull(sample);          // every dequeued frame has a sample
            samples.Add(sample!);
        }

        Assert.Equal(3, samples.Count);

        // Receive is present exactly for the frames wrapped in BeginAcquire, and absent otherwise — the whole
        // point of the opt-in: wait/queue for free, receive only when the source measures its fetch.
        Assert.NotNull(samples[0].AcquireMicros);
        Assert.Null(samples[1].AcquireMicros);
        Assert.NotNull(samples[2].AcquireMicros);
        Assert.True(samples[0].AcquireMicros > 0, "a measured fetch should record a positive receive time");

        // Wait and queue are always populated and never negative.
        Assert.All(samples, s =>
        {
            Assert.True(s.QueueMicros >= 0);
            Assert.True(s.WaitMicros >= 0);
        });
    }

    [Fact]
    public void AccumulatorAveragesReceiveOverReceivingFramesOnly()
    {
        var acc = new SourceAcquisitionAccumulator();
        acc.Record(new FrameAcquisitionSample { AcquireMicros = 1_000, QueueMicros = 200, WaitMicros = 5_000 });
        acc.Record(new FrameAcquisitionSample { AcquireMicros = null,  QueueMicros = 400, WaitMicros = 7_000 });
        acc.Record(new FrameAcquisitionSample { AcquireMicros = 3_000, QueueMicros = 600, WaitMicros = 9_000 });

        var p = acc.ToProfile();

        Assert.Equal(3, p.Frames);
        Assert.Equal(2, p.ReceiveFrames);
        Assert.True(p.HasReceive);

        Assert.Equal(3_000, p.LastReceiveMicros);
        Assert.Equal(600, p.LastQueueMicros);
        Assert.Equal(9_000, p.LastWaitMicros);

        // Receive averages over the two frames that reported it (2.0 ms), not over all three.
        Assert.Equal(2.0, p.AverageReceiveMs, 3);
        Assert.Equal(0.4, p.AverageQueueMs, 3);   // 1200 / 3 frames
        Assert.Equal(7.0, p.AverageWaitMs, 3);    // 21000 / 3 frames
    }

    [Fact]
    public void ProfileHasNoReceiveWhenNoFrameOptedIn()
    {
        var acc = new SourceAcquisitionAccumulator();
        acc.Record(new FrameAcquisitionSample { AcquireMicros = null, QueueMicros = 100, WaitMicros = 4_000 });

        var p = acc.ToProfile();

        Assert.Equal(1, p.Frames);
        Assert.False(p.HasReceive);
        Assert.Equal(0, p.AverageReceiveMs);
    }
}
