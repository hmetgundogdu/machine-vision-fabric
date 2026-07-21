namespace Mvf.Graph.Execution;

/// <summary>
/// Lifecycle status of a managed pipeline execution host.
/// </summary>
public enum PipelineExecutionStatus
{
    /// <summary>No pipeline has been started yet.</summary>
    Idle = 0,

    /// <summary>Nodes are being activated.</summary>
    Starting = 1,

    /// <summary>Pipeline is actively executing cycles.</summary>
    Running = 2,

    /// <summary>A stop was requested; waiting for the current cycle to finish.</summary>
    Stopping = 3,

    /// <summary>Execution completed normally (source exhausted or MaxCycles reached).</summary>
    Stopped = 4,

    /// <summary>Execution terminated due to an unhandled error.</summary>
    Faulted = 5
}
