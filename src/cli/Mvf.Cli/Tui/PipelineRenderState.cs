using Mvf.Graph.Execution;
using Mvf.Graph.Pipelines;

namespace Mvf.Cli.Tui;

/// <summary>
/// Thread-safe, shared render state updated by executor callbacks and read by the TUI renderer.
/// </summary>
public sealed class PipelineRenderState
{
    private readonly object _lock = new();
    private readonly CircularBuffer<PipelineLogEntry> _logs;
    private readonly Dictionary<string, PipelineNodeState> _nodes;

    public const int DefaultLogCapacity = 20;

    public PipelineRenderState(PipelineDefinition definition, int logCapacity = DefaultLogCapacity)
    {
        _logs = new CircularBuffer<PipelineLogEntry>(logCapacity);
        _nodes = definition.Nodes
            .ToDictionary(
                n => n.Id,
                n => new PipelineNodeState
                {
                    NodeId = n.Id,
                    DisplayName = n.DisplayName ?? n.Id,
                    Category = n.Category ?? string.Empty,
                    Kind = n.Kind,
                    TypeLabel = ResolveTypeLabel(n)
                },
                StringComparer.OrdinalIgnoreCase);
    }

    // ── Public state (immutable snapshots) ────────────────────────────────

    public string? RunId { get; private set; }
    public string PipelineName { get; private set; } = string.Empty;
    public int TotalCycles { get; private set; }
    public int AcceptedCycles { get; private set; }
    public TimeSpan Elapsed { get; private set; }
    public bool IsFinished { get; private set; }

    public IReadOnlyDictionary<string, PipelineNodeState> Nodes => _nodes;

    // ── Mutation API (called from executor callbacks) ──────────────────────

    public void OnRunStarted(string runId, string pipelineName)
    {
        lock (_lock)
        {
            RunId = runId;
            PipelineName = pipelineName;
            AddLog(LogLevel.Info, $"Run started — id:{runId}");
        }
    }

    public void OnNodeExecuted(NodeExecutionEvent e)
    {
        lock (_lock)
        {
            if (!_nodes.TryGetValue(e.NodeId, out var node)) return;

            node.Status = e.Faulted ? NodeLifecycleStatus.Faulted : NodeLifecycleStatus.Done;
            node.TotalCycles++;
            node.LastDurationMs = e.DurationMs;
            node.LastInputPorts = e.InputPortNames;
            node.LastOutputPorts = e.OutputPortNames;

            if (e.HasOutput && !e.Faulted)
                node.AcceptedCycles++;
            if (e.Faulted)
                node.FaultedCycles++;

            // A supervised restart is invisible to the graph — the cycle just succeeds on the retry. Log
            // the moment the count moves so recovery is something the operator sees, not something they
            // infer from a latency spike.
            if (e.WorkerRestarts > node.WorkerRestarts)
            {
                var recovered = e.WorkerRestarts - node.WorkerRestarts;
                node.WorkerRestarts = e.WorkerRestarts;
                AddLog(LogLevel.Warning,
                    $"[cyc:{e.CycleIndex}] {e.NodeId}  worker restarted x{recovered} (recovered, total {e.WorkerRestarts})");
            }

            var ports = e.HasOutput
                ? $"→ [{string.Join(", ", e.OutputPortNames)}]"
                : "→ (no output)";
            var lvl = e.Faulted ? LogLevel.Error : LogLevel.Info;
            AddLog(lvl, $"[cyc:{e.CycleIndex}] {e.NodeId}  {ports}  ({e.DurationMs}ms)");
        }
    }

    public void OnCycleCompleted(PipelineExecutionProgress p)
    {
        lock (_lock)
        {
            TotalCycles = p.TotalCycles;
            AcceptedCycles = p.AcceptedCycles;
            Elapsed = p.Elapsed;

            // Reset Active → Done for all nodes each cycle boundary
            foreach (var node in _nodes.Values)
            {
                if (node.Status == NodeLifecycleStatus.Active)
                    node.Status = NodeLifecycleStatus.Done;
            }

        var icon = p.CycleAccepted ? "[OK]" : "[--]";
            AddLog(LogLevel.Success,
                $"[cyc:{p.CycleIndex}] {icon}  total={p.TotalCycles} accepted={p.AcceptedCycles}  elapsed={p.Elapsed.TotalSeconds:F1}s");
        }
    }

    public void OnFinished(string? error)
    {
        lock (_lock)
        {
            IsFinished = true;
            if (error is not null)
                AddLog(LogLevel.Error, $"Run ended with error: {error}");
            else
                AddLog(LogLevel.Success, $"Run complete — {TotalCycles} cycles, {AcceptedCycles} accepted, {Elapsed.TotalSeconds:F2}s");
        }
    }

    /// <summary>Returns a snapshot of recent log entries (newest last).</summary>
    public IReadOnlyList<PipelineLogEntry> GetLogs(int maxCount)
    {
        lock (_lock)
        {
            return _logs.TakeLast(maxCount);
        }
    }

    /// <summary>Returns a shallow copy of node states for safe rendering.</summary>
    public IReadOnlyList<PipelineNodeState> GetNodeSnapshot()
    {
        lock (_lock)
        {
            return _nodes.Values.ToList();
        }
    }

    private void AddLog(LogLevel level, string message) =>
        _logs.Add(new PipelineLogEntry { Timestamp = DateTime.Now, Level = level, Message = message });

    private static string ResolveTypeLabel(PipelineNodeDefinition n) =>
        n.Kind switch
        {
            "integration-module" => n.ModuleId ?? "module",
            "embedded-primitive" => n.PrimitiveType ?? "primitive",
            "runtime-builtin" => n.BuiltinType ?? "builtin",
            _ => n.Kind
        };
}

/// <summary>Fixed-capacity circular buffer. Not thread-safe — callers must lock.</summary>
internal sealed class CircularBuffer<T>(int capacity)
{
    private readonly T[] _buffer = new T[capacity];
    private int _head;
    private int _count;

    public void Add(T item)
    {
        _buffer[_head % capacity] = item;
        _head++;
        if (_count < capacity) _count++;
    }

    public IReadOnlyList<T> TakeLast(int n)
    {
        var take = Math.Min(n, _count);
        var result = new T[take];
        var start = _head - take;
        for (var i = 0; i < take; i++)
        {
            result[i] = _buffer[(start + i + capacity) % capacity];
        }
        return result;
    }
}
