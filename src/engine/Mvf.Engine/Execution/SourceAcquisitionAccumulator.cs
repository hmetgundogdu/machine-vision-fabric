using Mvf.Graph.Execution;

namespace Mvf.Engine.Execution;

/// <summary>
/// Rolls up a source node's per-frame <see cref="FrameAcquisitionSample"/>s into a <see cref="SourceProfile"/>
/// over a run. Shared by both executors so the serial and pipelined paths surface acquisition timing the same
/// way. A source is a single instance (not replicated), so the owning stage thread is the only writer — totals
/// still use interlocked adds so a stray concurrent read never sees a torn long.
/// </summary>
internal sealed class SourceAcquisitionAccumulator
{
    private long _frames;
    private long _receiveFrames;
    private long _lastReceiveMicros;
    private long _lastQueueMicros;
    private long _lastWaitMicros;
    private long _totalReceiveMicros;
    private long _totalQueueMicros;
    private long _totalWaitMicros;

    public void Record(FrameAcquisitionSample sample)
    {
        Interlocked.Increment(ref _frames);
        Interlocked.Add(ref _totalQueueMicros, sample.QueueMicros);
        Interlocked.Add(ref _totalWaitMicros, sample.WaitMicros);
        Interlocked.Exchange(ref _lastQueueMicros, sample.QueueMicros);
        Interlocked.Exchange(ref _lastWaitMicros, sample.WaitMicros);

        if (sample.AcquireMicros is { } receive)
        {
            Interlocked.Increment(ref _receiveFrames);
            Interlocked.Add(ref _totalReceiveMicros, receive);
            Interlocked.Exchange(ref _lastReceiveMicros, receive);
        }
    }

    public SourceProfile ToProfile() => new()
    {
        Frames = Interlocked.Read(ref _frames),
        ReceiveFrames = Interlocked.Read(ref _receiveFrames),
        LastReceiveMicros = Interlocked.Read(ref _lastReceiveMicros),
        LastQueueMicros = Interlocked.Read(ref _lastQueueMicros),
        LastWaitMicros = Interlocked.Read(ref _lastWaitMicros),
        TotalReceiveMicros = Interlocked.Read(ref _totalReceiveMicros),
        TotalQueueMicros = Interlocked.Read(ref _totalQueueMicros),
        TotalWaitMicros = Interlocked.Read(ref _totalWaitMicros)
    };
}
