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

    public IReadOnlyList<string> LastInputPorts { get; set; } = [];
    public IReadOnlyList<string> LastOutputPorts { get; set; } = [];
}

/// <summary>
/// Renders a node duration. A local stage is routinely sub-millisecond, so whole milliseconds would
/// show most of the graph as "0ms"; below 10ms one decimal is kept.
/// </summary>
internal static class DurationText
{
    public static string Format(long micros) =>
        micros < 10_000 ? $"{micros / 1000.0:F1}ms" : $"{micros / 1000}ms";
}
