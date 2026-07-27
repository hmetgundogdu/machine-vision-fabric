using System.Diagnostics;
using System.Runtime.CompilerServices;
using System.Threading.Channels;
using Mvf.Abstractions;
using Mvf.Graph.Execution;

namespace Mvf.Sdk;

public abstract class BackgroundFrameSourceSession : IFrameSourceSession, ISourceAcquisitionMetrics
{
    /// <summary>A queued frame plus the timing the base session stamps onto it, so acquisition metrics need
    /// no change to <see cref="IFrameEnvelope"/> (a version-pinned contract external modules implement).</summary>
    private readonly record struct QueuedFrame(IFrameEnvelope Frame, long EnqueuedTimestamp, long? AcquireMicros, long? FreshnessMicros);

    private readonly Channel<QueuedFrame> channel;
    private readonly CancellationTokenSource disposeCts = new();
    private Task? producerTask;

    // Producer-thread only (SingleWriter): the receive span of the current in-flight fetch, set when a
    // BeginAcquire scope closes and consumed by the next PublishAsync.
    private long? pendingAcquireMicros;

    // Reader-thread only (SingleReader): timestamp of the previous dequeue, so wait is the gap between frames.
    private long lastDequeueTimestamp;

    // Written by the reader, read by the engine on the same stage thread right after ExecuteAsync. volatile
    // guards the one cross-async-boundary hand-off; the object itself is immutable.
    private volatile FrameAcquisitionSample? lastAcquisition;

    protected BackgroundFrameSourceSession(int declaredCameraCount, int? estimatedFrameCount, int boundedCapacity = 8)
    {
        DeclaredCameraCount = Math.Max(1, declaredCameraCount);
        EstimatedFrameCount = estimatedFrameCount;

        // The first frame's wait is measured from construction — it legitimately includes connect/startup
        // (a slow first frame is a slow first frame); steady-state warmup is tracked separately as WarmupMs.
        lastDequeueTimestamp = Stopwatch.GetTimestamp();

        channel = Channel.CreateBounded<QueuedFrame>(new BoundedChannelOptions(Math.Max(1, boundedCapacity))
        {
            SingleReader = true,
            SingleWriter = true,
            FullMode = BoundedChannelFullMode.Wait
        });
    }

    public int DeclaredCameraCount { get; }

    public int? EstimatedFrameCount { get; }

    public async IAsyncEnumerable<IFrameEnvelope> ReadFramesAsync([EnumeratorCancellation] CancellationToken cancellationToken)
    {
        await foreach (var queued in channel.Reader.ReadAllAsync(cancellationToken))
        {
            var now = Stopwatch.GetTimestamp();
            lastAcquisition = new FrameAcquisitionSample
            {
                AcquireMicros = queued.AcquireMicros,
                QueueMicros = ElapsedMicros(queued.EnqueuedTimestamp, now),
                WaitMicros = ElapsedMicros(lastDequeueTimestamp, now),
                FreshnessMicros = queued.FreshnessMicros
            };
            lastDequeueTimestamp = now;

            yield return queued.Frame;
        }
    }

    /// <summary>The timing of the most recently dequeued frame, for the engine to poll after ExecuteAsync.</summary>
    public FrameAcquisitionSample? GetLastAcquisition() => lastAcquisition;

    public async ValueTask DisposeAsync()
    {
        disposeCts.Cancel();

        if (producerTask is not null)
        {
            try
            {
                await producerTask;
            }
            catch
            {
                // A producer fault was already delivered to the reader via TryComplete(ex) — that is the
                // path the engine observes. Rethrowing it here would only turn cleanup into a second,
                // duplicate failure (and mask whatever the caller was disposing for).
            }
        }

        disposeCts.Dispose();
    }

    protected void StartBackgroundProducer(Func<CancellationToken, Task> producer)
    {
        if (producerTask is not null)
        {
            throw new InvalidOperationException("Background producer has already been started.");
        }

        producerTask = Task.Run(async () =>
        {
            try
            {
                await producer(disposeCts.Token);
                channel.Writer.TryComplete();
            }
            catch (OperationCanceledException)
            {
                channel.Writer.TryComplete();
                throw;
            }
            catch (Exception ex)
            {
                channel.Writer.TryComplete(ex);
            }
        }, disposeCts.Token);
    }

    protected ValueTask PublishAsync(IFrameEnvelope frame, CancellationToken cancellationToken)
    {
        // Consume the receive span measured by the most recent BeginAcquire scope (null if the source did
        // not opt in). One acquire pairs with the next publish; reset so it never leaks onto a later frame.
        var acquire = pendingAcquireMicros;
        pendingAcquireMicros = null;

        var queued = new QueuedFrame(frame, Stopwatch.GetTimestamp(), acquire, FreshnessMicros(frame));
        return channel.Writer.WriteAsync(queued, cancellationToken);
    }

    /// <summary>
    /// Opens a scope that measures how long acquiring one frame takes — the receive time the pipeline
    /// otherwise never sees, because the fetch runs on this producer thread, not inside the node's
    /// <c>ExecuteAsync</c>. Wrap the actual device read / HTTP fetch:
    /// <code>using (BeginAcquire()) { /* fetch bytes */ } await PublishAsync(frame, ct);</code>
    /// The measured span is attached to the next <see cref="PublishAsync"/>. Optional — wait and queue age
    /// are reported without it; this is the one line that unlocks the receive number.
    /// </summary>
    protected AcquireScope BeginAcquire() => new(this);

    /// <summary>Disposable receive-timing scope; see <see cref="BeginAcquire"/>. A struct, so it never allocates.</summary>
    protected readonly struct AcquireScope : IDisposable
    {
        private readonly BackgroundFrameSourceSession _session;
        private readonly long _start;

        internal AcquireScope(BackgroundFrameSourceSession session)
        {
            _session = session;
            _start = Stopwatch.GetTimestamp();
        }

        public void Dispose() => _session.pendingAcquireMicros = ElapsedMicros(_start, Stopwatch.GetTimestamp());
    }

    private static long ElapsedMicros(long fromTimestamp, long toTimestamp) =>
        (long)(Math.Max(0, toTimestamp - fromTimestamp) * (1_000_000.0 / Stopwatch.Frequency));

    /// <summary>Glass-to-fabric lag from the frame's own capture time to now, or null when the timestamp is
    /// absent/implausible (default, or in the future) — most sources stamp creation time, so this is best-effort.</summary>
    private static long? FreshnessMicros(IFrameEnvelope frame)
    {
        var captured = frame.TimestampUtc;
        if (captured == default) return null;
        var lag = DateTime.UtcNow - captured;
        return lag < TimeSpan.Zero ? null : (long)lag.TotalMicroseconds;
    }
}
