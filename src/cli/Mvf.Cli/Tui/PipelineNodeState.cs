namespace Mvf.Cli.Tui;

public enum NodeLifecycleStatus
{
    Idle,
    Active,   // currently executing this cycle
    Done,
    Faulted
}

/// <summary>Per-node runtime state used for TUI rendering.</summary>
public sealed class PipelineNodeState
{
    public required string NodeId { get; init; }
    public required string DisplayName { get; init; }
    public required string Category { get; init; }    // source/control/flow-control/output
    public required string Kind { get; init; }        // integration-module/embedded-primitive/runtime-builtin
    public required string TypeLabel { get; init; }   // moduleId / primitiveType / builtinType

    public NodeLifecycleStatus Status { get; set; } = NodeLifecycleStatus.Idle;
    public int TotalCycles { get; set; }
    public int AcceptedCycles { get; set; }
    public int FaultedCycles { get; set; }
    public long LastDurationMicros { get; set; }

    // Rolling timing: kept alongside LastDurationMicros so the detail view can show min/avg/max without a
    // second pass over the log. Accumulated in PipelineRenderState.OnNodeExecuted.
    public long TotalDurationMicros { get; set; }
    public long MinDurationMicros { get; set; } = long.MaxValue;
    public long MaxDurationMicros { get; set; }

    /// <summary>Mean per-cycle duration in microseconds, or 0 before the first cycle.</summary>
    public double AverageDurationMicros => TotalCycles > 0 ? (double)TotalDurationMicros / TotalCycles : 0;

    /// <summary>Out-of-process worker restarts observed so far for this node (0 when it runs in-process).</summary>
    public int WorkerRestarts { get; set; }

    /// <summary>
    /// <see cref="System.Environment.TickCount64"/> at the node's most recent execution. Drives the
    /// graph's afterglow: a node that ran a moment ago fades from bright back to its category colour, so
    /// the execution wave is visible sweeping through the graph. 0 until the node first runs.
    /// </summary>
    public long LastActiveTicks { get; set; }

    public IReadOnlyList<string> LastInputPorts { get; set; } = [];
    public IReadOnlyList<string> LastOutputPorts { get; set; } = [];

    // ── Source acquisition (populated only for a source node that reports it) ──
    // Kept alongside the duration stats so the box can headline the receive time and the detail view can
    // show wait/recv/queue with a rolling average, without waiting for the run's final report.
    public long AcqFrames { get; set; }          // frames that reported a sample (wait/queue)
    public long AcqReceiveFrames { get; set; }   // frames that also reported a receive time (source opted in)
    public long LastReceiveMicros { get; set; }
    public long LastQueueMicros { get; set; }
    public long LastWaitMicros { get; set; }
    public long TotalReceiveMicros { get; set; }
    public long TotalQueueMicros { get; set; }
    public long TotalWaitMicros { get; set; }

    /// <summary>True once this node has reported any acquisition sample — i.e. it is an instrumented source.</summary>
    public bool HasAcquisition => AcqFrames > 0;

    /// <summary>True once a frame reported a receive time (the source wrapped its fetch in <c>BeginAcquire()</c>).</summary>
    public bool HasReceive => AcqReceiveFrames > 0;

    public double AverageReceiveMicros => AcqReceiveFrames > 0 ? (double)TotalReceiveMicros / AcqReceiveFrames : 0;
    public double AverageQueueMicros => AcqFrames > 0 ? (double)TotalQueueMicros / AcqFrames : 0;
    public double AverageWaitMicros => AcqFrames > 0 ? (double)TotalWaitMicros / AcqFrames : 0;

    // ── Worker resource usage (out-of-process / worker nodes only) ──
    // Populated only when the node reports a worker snapshot; an in-process node leaves HasWorker false.
    // Peak is the largest working set the dashboard has observed this run (samples are throttled to ~500ms).
    public bool HasWorker { get; set; }
    public double LastWorkerCpuPercent { get; set; }
    public long LastWorkerWorkingSetBytes { get; set; }
    public long PeakWorkerWorkingSetBytes { get; set; }

    // ── Frame size (frame-producing nodes) ──
    public long FrameBytesCount { get; set; }
    public long LastFrameBytes { get; set; }
    public long TotalFrameBytes { get; set; }

    public bool HasFrameBytes => FrameBytesCount > 0;
    public double AverageFrameBytes => FrameBytesCount > 0 ? (double)TotalFrameBytes / FrameBytesCount : 0;
}

/// <summary>
/// Renders a duration compactly, scaling the unit to the magnitude so it stays readable and narrow from a
/// sub-millisecond stage to an hours-long run. A local stage is routinely sub-millisecond, so whole
/// milliseconds would show most of the graph as "0ms" — below 10ms one decimal is kept; past a minute the
/// value rolls up to <c>m s</c> / <c>h m</c> so a header like <c>t:2044.9s</c> becomes <c>t:34m 05s</c>.
/// </summary>
internal static class DurationText
{
    public static string Format(long micros)
    {
        if (micros < 10_000)    return $"{micros / 1000.0:F1}ms";  // <10ms — keep sub-ms detail
        if (micros < 1_000_000) return $"{micros / 1000}ms";       // <1s — whole milliseconds

        var seconds = micros / 1_000_000.0;
        if (seconds < 60) return $"{seconds:F1}s";                 // one decimal through a minute (12.3s)

        var totalSeconds = (long)Math.Round(seconds);
        if (totalSeconds < 3600)
            return $"{totalSeconds / 60}m {totalSeconds % 60:00}s";

        var totalMinutes = totalSeconds / 60;
        return $"{totalMinutes / 60}h {totalMinutes % 60:00}m";
    }

    /// <summary>Same scaling for a <see cref="TimeSpan"/> — the header's elapsed and per-cycle readouts.</summary>
    public static string Format(TimeSpan span) => Format((long)span.TotalMicroseconds);
}

/// <summary>
/// Renders a byte count compactly (B / K / M / G), for memory footprints and frame sizes. One decimal below
/// ten of a unit so a small frame or a lean worker still reads with precision (e.g. <c>301K</c>, <c>1.4G</c>).
/// </summary>
internal static class Bytes
{
    public static string Format(long bytes)
    {
        if (bytes < 1024) return $"{bytes}B";
        var kb = bytes / 1024.0;
        if (kb < 1024) return kb < 10 ? $"{kb:F1}K" : $"{kb:F0}K";
        var mb = kb / 1024.0;
        if (mb < 1024) return mb < 10 ? $"{mb:F1}M" : $"{mb:F0}M";
        return $"{mb / 1024.0:F1}G";
    }
}
