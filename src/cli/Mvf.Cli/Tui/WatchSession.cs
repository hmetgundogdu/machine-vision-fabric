using Mvf.Abstractions;
using Mvf.Egress;
using Mvf.Graph.Execution;
using Mvf.Graph.Pipelines;

namespace Mvf.Cli.Tui;

/// <summary>
/// The observer's live picture of one remote run, fed by decoded egress records.
///
/// <para>The point of this class is that a watched pipeline and a local one are rendered by the same code.
/// A <see cref="EgressStreamKind.Topology"/> record rebuilds the <see cref="PipelineDefinition"/>, which
/// builds the ordinary <see cref="PipelineRenderState"/>; every later record is turned back into the
/// <see cref="NodeExecutionEvent"/> / <see cref="PipelineExecutionProgress"/> the state already understands.
/// So the observer reuses <see cref="GraphRenderer"/> unchanged instead of growing a second renderer.</para>
///
/// <para>Records can arrive before the topology (a v1 producer never sends one, and UDP repeats it only
/// once a second), so node rows are also accumulated independently. That table is what the observer falls
/// back to when there is no graph to draw — and it is what makes attaching to an older engine still useful.</para>
/// </summary>
internal sealed class WatchSession
{
    /// <summary>The window used for the live rate readouts. Short enough to react, long enough not to jump.</summary>
    private static readonly TimeSpan RateWindow = TimeSpan.FromSeconds(2);

    private readonly object _gate = new();
    private readonly Dictionary<string, ObservedNode> _observed = new(StringComparer.OrdinalIgnoreCase);
    private readonly Queue<(DateTime At, long Bytes)> _rateSamples = new();

    private PipelineRenderState? _state;
    private DateTime? _firstRecordAt;

    /// <summary>The rebuilt definition once topology has arrived, else null.</summary>
    public PipelineDefinition? Definition { get; private set; }

    /// <summary>The layout for <see cref="Definition"/>, built once per topology.</summary>
    public GraphLayout? Layout { get; private set; }

    public string PipelineName { get; private set; } = string.Empty;

    public string RunId { get; private set; } = string.Empty;

    public long Records { get; private set; }

    public long Bytes { get; private set; }

    public long FrameRecords { get; private set; }

    public long FrameBytes { get; private set; }

    /// <summary>Records whose sequence number skipped — the producer dropped them, or the transport did.
    /// Shown rather than hidden: on a best-effort stream loss is expected, and an observer that silently
    /// smooths over it would misreport the run.</summary>
    public long Gaps { get; private set; }

    public uint LastCycle { get; private set; }

    public uint TotalCycles { get; private set; }

    public uint AcceptedCycles { get; private set; }

    public TimeSpan Elapsed { get; private set; }

    /// <summary>What the last frame-data record carried, for the frame panel. Null until one arrives.</summary>
    public FrameInfo? LastFrame { get; private set; }

    public DateTime? LastRecordAt { get; private set; }

    private uint _lastSeq;
    private bool _hasSeq;

    private string? _payloadInterest;

    /// <summary>
    /// Which node's frame bytes to retain, if any — set when the detail page opens and cleared when it
    /// closes. This is what keeps the observer's memory bounded while still letting one node's payload be
    /// inspected; a viewer that quietly held every frame would be the heaviest thing on the panel PC.
    /// </summary>
    public void SetPayloadInterest(string? nodeId)
    {
        lock (_gate)
        {
            if (string.Equals(_payloadInterest, nodeId, StringComparison.OrdinalIgnoreCase))
            {
                return;
            }

            if (_payloadInterest is { } previous && _observed.TryGetValue(previous, out var node))
            {
                node.DropPayload();
            }

            _payloadInterest = nodeId;
        }
    }

    /// <summary>The observed row for one node, or null when nothing has been seen from it.</summary>
    public ObservedNode? Node(string nodeId)
    {
        lock (_gate)
        {
            return _observed.GetValueOrDefault(nodeId);
        }
    }

    /// <summary>The render state, once topology has arrived.</summary>
    public PipelineRenderState? State
    {
        get { lock (_gate) { return _state; } }
    }

    /// <summary>A stable, sorted view of the nodes seen so far — the fallback table.</summary>
    public IReadOnlyList<ObservedNode> ObservedNodes
    {
        get
        {
            lock (_gate)
            {
                return [.. _observed.Values.OrderByDescending(n => n.LastSeenTicks)];
            }
        }
    }

    /// <summary>Records per second over the recent window.</summary>
    public double RecordsPerSecond { get; private set; }

    /// <summary>Bytes per second over the recent window — the number that says whether a consumer can keep
    /// up with what the pipeline publishes.</summary>
    public double BytesPerSecond { get; private set; }

    /// <summary>Folds one decoded record into the picture. Called from the consume loop only.</summary>
    public void Apply(DecodedEgressRecord record, int wireBytes)
    {
        ArgumentNullException.ThrowIfNull(record);

        lock (_gate)
        {
            var now = DateTime.UtcNow;
            _firstRecordAt ??= now;
            LastRecordAt = now;
            Records++;
            Bytes += wireBytes;
            TrackRate(now, wireBytes);

            // Sequence gaps: topology is replayed with seq 0 per subscriber, so it is not part of the
            // sequence and must not be counted as a jump.
            if (record.Kind != EgressStreamKind.Topology)
            {
                if (_hasSeq && record.Seq > _lastSeq + 1)
                {
                    Gaps += record.Seq - _lastSeq - 1;
                }

                _lastSeq = record.Seq;
                _hasSeq = true;
            }

            if (record.RunId != Guid.Empty)
            {
                var runId = record.RunId.ToString("N");
                if (RunId.Length == 0)
                {
                    RunId = runId;
                }
                else if (!string.Equals(RunId, runId, StringComparison.Ordinal))
                {
                    // The producer restarted: the graph may differ, so start from a blank picture rather
                    // than blending two runs' counters into one misleading view.
                    ResetForNewRun(runId);
                }
            }

            switch (record.Kind)
            {
                case EgressStreamKind.Topology:
                    ApplyTopology(record.Topology);
                    break;
                case EgressStreamKind.NodeTransition:
                    ApplyNodeTransition(record, now);
                    break;
                default:
                    ApplyCycle(record);
                    break;
            }
        }
    }

    private void ResetForNewRun(string runId)
    {
        RunId = runId;
        _observed.Clear();
        _state = null;
        Definition = null;
        Layout = null;
        LastFrame = null;
        TotalCycles = 0;
        AcceptedCycles = 0;
        LastCycle = 0;
        Elapsed = TimeSpan.Zero;
        _hasSeq = false;
    }

    private void ApplyTopology(EgressTopology? topology)
    {
        if (topology is null)
        {
            return;
        }

        PipelineName = topology.Name;

        // Rebuild only when the shape actually changed; a UDP producer repeats this record every second and
        // rebuilding would throw away accumulated per-node state each time.
        var signature = TopologySignature(topology);
        if (signature == _topologySignature)
        {
            return;
        }

        _topologySignature = signature;

        // Carry the human names over to the observed rows: the table is read by someone looking for
        // "Brightness", not for "bright".
        foreach (var n in topology.Nodes)
        {
            if (!_observed.TryGetValue(n.Id, out var observed))
            {
                observed = new ObservedNode(n.Id);
                _observed[n.Id] = observed;
            }

            observed.Describe(n);
        }

        Definition = topology.ToDefinition();
        Layout = GraphLayout.Build(Definition);
        var state = new PipelineRenderState(Definition);
        state.OnRunStarted(topology.RunId.Length >= 8 ? topology.RunId[..8] : topology.RunId, topology.Name);
        _state = state;

        // Replay what was already seen so a late topology does not lose the nodes observed before it.
        foreach (var node in _observed.Values)
        {
            state.OnNodeExecuted(node.ToEvent(RunId));
        }
    }

    private string? _topologySignature;

    private static string TopologySignature(EgressTopology t) =>
        string.Join('|', t.Nodes.Select(n => n.Id)) + "//" +
        string.Join('|', t.Edges.Select(e => $"{e.FromNode}.{e.FromPort}>{e.ToNode}.{e.ToPort}"));

    private void ApplyNodeTransition(DecodedEgressRecord record, DateTime now)
    {
        if (!_observed.TryGetValue(record.NodeId, out var node))
        {
            node = new ObservedNode(record.NodeId);
            _observed[record.NodeId] = node;
        }

        node.Observe(record, now);

        if (record.HasPayload || record.PayloadShape is not null)
        {
            FrameRecords++;
            FrameBytes += record.Payload?.Length ?? 0;

            var info = new FrameInfo(
                record.NodeId,
                record.Port,
                record.MediaType,
                record.ElementType,
                record.PayloadShape,
                record.Payload?.Length ?? 0,
                now);

            // Metadata for every node (cheap), bytes for at most one. Retaining every node's last frame
            // would mean 13 nodes x a multi-megabyte frame resident in a viewer that exists to be light;
            // the detail page only ever shows one node, so only that node's bytes are kept.
            node.RememberFrame(info, string.Equals(_payloadInterest, record.NodeId, StringComparison.OrdinalIgnoreCase)
                ? record.Payload
                : null);
            LastFrame = info;
        }

        _state?.OnNodeExecuted(node.ToEvent(RunId, record));
    }

    private void ApplyCycle(DecodedEgressRecord record)
    {
        LastCycle = record.CycleIndex;
        TotalCycles = record.TotalCycles;
        AcceptedCycles = record.AcceptedCycles;
        Elapsed = TimeSpan.FromMilliseconds(record.ElapsedMillis);

        _state?.OnCycleCompleted(new PipelineExecutionProgress
        {
            RunId = RunId,
            CycleIndex = (int)record.CycleIndex,
            TotalCycles = (int)record.TotalCycles,
            AcceptedCycles = (int)record.AcceptedCycles,
            CycleAccepted = record.Accepted,
            Elapsed = Elapsed,
        });
    }

    private void TrackRate(DateTime now, int bytes)
    {
        _rateSamples.Enqueue((now, bytes));
        var cutoff = now - RateWindow;
        while (_rateSamples.Count > 0 && _rateSamples.Peek().At < cutoff)
        {
            _rateSamples.Dequeue();
        }

        if (_rateSamples.Count < 2)
        {
            return;
        }

        var span = (now - _rateSamples.Peek().At).TotalSeconds;
        if (span <= 0)
        {
            return;
        }

        RecordsPerSecond = _rateSamples.Count / span;
        BytesPerSecond = _rateSamples.Sum(s => s.Bytes) / span;
    }

    /// <summary>What the last frame-data record described.</summary>
    internal sealed record FrameInfo(
        string NodeId,
        string Port,
        PayloadMediaType? MediaType,
        PayloadElementType? ElementType,
        long[]? Shape,
        int Bytes,
        DateTime At)
    {
        public string ShapeText => Shape is { Length: > 0 } ? string.Join("x", Shape) : "-";
    }

    /// <summary>One node as the observer has seen it — the fallback table's row, and the source for the
    /// synthetic events that drive the shared render state.</summary>
    internal sealed class ObservedNode(string nodeId)
    {
        public string NodeId { get; } = nodeId;

        /// <summary>The node's human name, once topology names it; the id until then.</summary>
        public string DisplayName { get; private set; } = nodeId;

        /// <summary>module id / primitive / builtin — whichever the node's kind selects.</summary>
        public string TypeLabel { get; private set; } = string.Empty;

        public string Kind { get; private set; } = string.Empty;

        public string Category { get; private set; } = string.Empty;

        /// <summary>The last frame this node emitted, or null when it has emitted none.</summary>
        public FrameInfo? LastFrame { get; private set; }

        /// <summary>The last frame's bytes — only ever populated while this node is the payload interest.</summary>
        public byte[]? LastPayload { get; private set; }

        public void Describe(EgressTopologyNode n)
        {
            DisplayName = n.DisplayName is { Length: > 0 } ? n.DisplayName : n.Id;
            Kind = n.Kind;
            Category = n.Category;
            TypeLabel = n.Kind switch
            {
                "integration-module" => n.ModuleId ?? "module",
                "embedded-primitive" => n.PrimitiveType ?? "primitive",
                "runtime-builtin" => n.BuiltinType ?? "builtin",
                _ => n.Kind,
            };
        }

        public void RememberFrame(FrameInfo info, byte[]? payload)
        {
            LastFrame = info;
            if (payload is not null)
            {
                LastPayload = payload;
            }
        }

        public void DropPayload() => LastPayload = null;

        public string LastPort { get; private set; } = string.Empty;

        public int Cycles { get; private set; }

        public int Faults { get; private set; }

        public long LastDurationMicros { get; private set; }

        public long TotalDurationMicros { get; private set; }

        public long LastFrameBytes { get; private set; }

        public int LastCycleIndex { get; private set; }

        public bool LastFaulted { get; private set; }

        public bool LastHasOutput { get; private set; }

        public long LastSeenTicks { get; private set; }

        public double AverageDurationMicros => Cycles > 0 ? (double)TotalDurationMicros / Cycles : 0;

        public void Observe(DecodedEgressRecord record, DateTime now)
        {
            LastPort = record.Port;
            Cycles++;
            if (record.Faulted) Faults++;
            LastFaulted = record.Faulted;
            LastHasOutput = record.HasOutput;
            LastDurationMicros = record.DurationMicros;
            TotalDurationMicros += record.DurationMicros;
            LastFrameBytes = record.OutputFrameBytes;
            LastCycleIndex = (int)record.CycleIndex;
            LastSeenTicks = now.Ticks;
        }

        /// <summary>Rebuilds the event this row came from, so the shared render state can consume it.</summary>
        public NodeExecutionEvent ToEvent(string runId, DecodedEgressRecord? record = null) => new()
        {
            RunId = runId,
            NodeId = NodeId,
            CycleIndex = record is null ? LastCycleIndex : (int)record.CycleIndex,
            HasOutput = record?.HasOutput ?? LastHasOutput,
            Faulted = record?.Faulted ?? LastFaulted,
            DurationMicros = record?.DurationMicros ?? LastDurationMicros,
            OutputPortNames = (record?.Port ?? LastPort) is { Length: > 0 } port ? [port] : [],
            InputPortNames = [],
            // The wire encodes "no frame" as -1 (it has no null). Handing that straight to the renderer
            // prints a literal "-1B" in the node box, so it is turned back into the absence it means.
            OutputFrameBytes = (record?.OutputFrameBytes ?? LastFrameBytes) is var bytes and > 0 ? bytes : null,
        };
    }
}
