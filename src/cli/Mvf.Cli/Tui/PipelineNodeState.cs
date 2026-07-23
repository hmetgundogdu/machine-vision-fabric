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
    public long LastDurationMs { get; set; }

    /// <summary>Out-of-process worker restarts observed so far for this node (0 when it runs in-process).</summary>
    public int WorkerRestarts { get; set; }

    public IReadOnlyList<string> LastInputPorts { get; set; } = [];
    public IReadOnlyList<string> LastOutputPorts { get; set; } = [];
}
