using MachineVisionFabric.Contracts.Execution;
using MachineVisionFabric.Contracts.Pipelines;
using MachineVisionFabric.Core.Abstractions;

namespace MachineVisionFabric.Runtime.Execution;

/// <summary>
/// Manages the lifecycle of a pipeline execution run.
/// Wraps <see cref="PipelineGraphExecutor"/> with start/stop/snapshot semantics.
///
/// Thread-safety: <see cref="GetSnapshot"/> is lock-free (volatile reads).
/// <see cref="StartAsync"/> and <see cref="StopAsync"/> are guarded by a simple lock.
/// </summary>
public sealed class PipelineExecutionHost(IPipelineGraphExecutor executor) : IPipelineExecutionHost
{
    private readonly object _lock = new();

    // Volatile snapshot state — updated by progress callback from the executor loop
    private volatile SnapshotState _state = new(PipelineExecutionStatus.Idle);

    private CancellationTokenSource? _cts;
    private Task<PipelineExecutionReport>? _runTask;

    public Task StartAsync(
        PipelineDefinition definition,
        PipelineExecutionOptions options,
        CancellationToken cancellationToken = default)
    {
        ArgumentNullException.ThrowIfNull(definition);
        ArgumentNullException.ThrowIfNull(options);

        lock (_lock)
        {
            var current = _state.Status;
            if (current is PipelineExecutionStatus.Running or PipelineExecutionStatus.Starting)
            {
                throw new InvalidOperationException(
                    $"A pipeline run is already in progress (status: {current}). Stop the current run first.");
            }

            var runId = Guid.NewGuid().ToString("N");
            var startedAt = DateTime.UtcNow;

            _state = new SnapshotState(PipelineExecutionStatus.Starting)
            {
                RunId = runId,
                PipelineName = definition.Name,
                StartedAt = startedAt
            };

            _cts = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);

            // Inject progress callback so snapshot stays current during execution
            var enrichedOptions = new PipelineExecutionOptions
            {
                PackageRoot = options.PackageRoot,
                IntegrationsRoot = options.IntegrationsRoot,
                MaxCycles = options.MaxCycles,
                OnCycleCompleted = progress =>
                {
                    _state = new SnapshotState(PipelineExecutionStatus.Running)
                    {
                        RunId = progress.RunId,
                        PipelineName = definition.Name,
                        TotalCycles = progress.TotalCycles,
                        AcceptedCycles = progress.AcceptedCycles,
                        StartedAt = startedAt,
                        Elapsed = progress.Elapsed
                    };

                    // Forward to caller's callback if provided
                    options.OnCycleCompleted?.Invoke(progress);
                }
            };

            _state = new SnapshotState(PipelineExecutionStatus.Running)
            {
                RunId = runId,
                PipelineName = definition.Name,
                StartedAt = startedAt
            };

            _runTask = Task.Run(async () =>
            {
                try
                {
                    var report = await executor.ExecuteAsync(definition, enrichedOptions, _cts.Token);
                    _state = new SnapshotState(PipelineExecutionStatus.Stopped)
                    {
                        RunId = runId,
                        PipelineName = definition.Name,
                        TotalCycles = report.TotalCycles,
                        AcceptedCycles = report.AcceptedCycles,
                        StartedAt = startedAt,
                        Elapsed = report.Duration
                    };
                    return report;
                }
                catch (OperationCanceledException)
                {
                    var elapsed = DateTime.UtcNow - startedAt;
                    var current2 = _state;
                    _state = new SnapshotState(PipelineExecutionStatus.Stopped)
                    {
                        RunId = runId,
                        PipelineName = definition.Name,
                        TotalCycles = current2.TotalCycles,
                        AcceptedCycles = current2.AcceptedCycles,
                        StartedAt = startedAt,
                        Elapsed = elapsed
                    };
                    return new PipelineExecutionReport
                    {
                        Succeeded = false,
                        TotalCycles = current2.TotalCycles,
                        AcceptedCycles = current2.AcceptedCycles,
                        Duration = elapsed,
                        ErrorMessage = "Run was cancelled."
                    };
                }
                catch (Exception ex)
                {
                    var elapsed = DateTime.UtcNow - startedAt;
                    var current2 = _state;
                    _state = new SnapshotState(PipelineExecutionStatus.Faulted)
                    {
                        RunId = runId,
                        PipelineName = definition.Name,
                        TotalCycles = current2.TotalCycles,
                        AcceptedCycles = current2.AcceptedCycles,
                        StartedAt = startedAt,
                        Elapsed = elapsed,
                        LastError = ex.Message
                    };
                    return new PipelineExecutionReport
                    {
                        Succeeded = false,
                        TotalCycles = current2.TotalCycles,
                        AcceptedCycles = current2.AcceptedCycles,
                        Duration = elapsed,
                        ErrorMessage = ex.Message
                    };
                }
            }, CancellationToken.None);

            return Task.CompletedTask;
        }
    }

    public Task StopAsync(CancellationToken cancellationToken = default)
    {
        lock (_lock)
        {
            if (_state.Status is PipelineExecutionStatus.Running or PipelineExecutionStatus.Starting)
            {
                var current = _state;
                _state = new SnapshotState(PipelineExecutionStatus.Stopping)
                {
                    RunId = current.RunId,
                    PipelineName = current.PipelineName,
                    TotalCycles = current.TotalCycles,
                    AcceptedCycles = current.AcceptedCycles,
                    StartedAt = current.StartedAt,
                    Elapsed = current.StartedAt.HasValue
                        ? DateTime.UtcNow - current.StartedAt.Value
                        : TimeSpan.Zero
                };
                _cts?.Cancel();
            }
        }

        return Task.CompletedTask;
    }

    public PipelineExecutionSnapshot GetSnapshot()
    {
        var s = _state;
        return new PipelineExecutionSnapshot
        {
            Status = s.Status,
            RunId = s.RunId,
            PipelineName = s.PipelineName,
            TotalCycles = s.TotalCycles,
            AcceptedCycles = s.AcceptedCycles,
            StartedAt = s.StartedAt,
            Elapsed = s.StartedAt.HasValue && s.Status == PipelineExecutionStatus.Running
                ? DateTime.UtcNow - s.StartedAt.Value
                : s.Elapsed,
            LastError = s.LastError
        };
    }

    public async Task<PipelineExecutionReport?> WaitForCompletionAsync(CancellationToken cancellationToken = default)
    {
        Task<PipelineExecutionReport>? task;
        lock (_lock)
        {
            task = _runTask;
        }

        if (task is null)
        {
            return null;
        }

        return await task.WaitAsync(cancellationToken);
    }

    public async ValueTask DisposeAsync()
    {
        await StopAsync();
        var task = _runTask;
        if (task is not null)
        {
            try { await task; } catch { /* swallow */ }
        }
        _cts?.Dispose();
    }

    // Mutable state bag. Replaced atomically via volatile write.
    private sealed class SnapshotState(PipelineExecutionStatus status)
    {
        public PipelineExecutionStatus Status { get; } = status;
        public string? RunId { get; init; }
        public string? PipelineName { get; init; }
        public int TotalCycles { get; init; }
        public int AcceptedCycles { get; init; }
        public DateTime? StartedAt { get; init; }
        public TimeSpan Elapsed { get; init; }
        public string? LastError { get; init; }
    }
}
