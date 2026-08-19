using System.Threading.Channels;
using Mvf.Graph.Execution;

namespace Mvf.Egress;

/// <summary>
/// The shared, transport-agnostic core of an egress sink: a bounded ring the publisher <c>TryWrite</c>s
/// into (dropping + counting when full, never blocking) and a single background drain that encodes each
/// record once and hands the bytes to a transport. Both <see cref="TcpServerEgressSink"/> and
/// <see cref="WebSocketEgressSink"/> compose this, so the off-the-hot-path guarantee and the wire encoding
/// live in exactly one place.
/// </summary>
internal sealed class EgressRecordQueue
{
    private readonly Channel<Queued> _channel;
    private long _published;
    private long _dropped;
    private uint _seq; // drain thread only — single reader, no interlock needed

    public EgressRecordQueue(int ringCapacity = 1024)
    {
        _channel = Channel.CreateBounded<Queued>(new BoundedChannelOptions(ringCapacity)
        {
            FullMode = BoundedChannelFullMode.Wait, // TryWrite returns false when full → count a drop
            SingleReader = true,
            SingleWriter = false,
        });
    }

    public long Published => Interlocked.Read(ref _published);

    public long Dropped => Interlocked.Read(ref _dropped);

    public void Enqueue(in Queued queued)
    {
        if (!_channel.Writer.TryWrite(queued))
        {
            Interlocked.Increment(ref _dropped);
        }
    }

    public void Complete() => _channel.Writer.TryComplete();

    /// <summary>Encodes each queued record and passes the framed bytes to <paramref name="broadcast"/>.</summary>
    public async Task DrainAsync(Func<byte[], Task> broadcast, CancellationToken cancellationToken)
    {
        var reader = _channel.Reader;
        try
        {
            while (await reader.WaitToReadAsync(cancellationToken).ConfigureAwait(false))
            {
                while (reader.TryRead(out var queued))
                {
                    var seq = _seq++;
                    var frame = Encode(queued, seq);
                    await broadcast(frame).ConfigureAwait(false);
                    Interlocked.Increment(ref _published);
                }
            }
        }
        catch (OperationCanceledException)
        {
        }
    }

    private static byte[] Encode(in Queued queued, uint seq) => queued.Kind switch
    {
        EgressStreamKind.Topology => EgressWire.EncodeTopology(queued.Topology!, queued.Topology!.RunId, seq),
        EgressStreamKind.Cycle => EgressWire.EncodeCycle(queued.Cycle!, seq),
        _ when queued.HasPayload => EgressWire.EncodeNodeTransitionFrame(
            queued.Node!, seq, queued.Descriptor.Span, queued.Payload.Span),
        _ => EgressWire.EncodeNodeTransition(queued.Node!, seq),
    };

    public readonly struct Queued
    {
        private Queued(
            EgressStreamKind kind,
            PipelineExecutionProgress? cycle,
            NodeExecutionEvent? node,
            bool hasPayload,
            ReadOnlyMemory<byte> descriptor,
            ReadOnlyMemory<byte> payload,
            EgressTopology? topology = null)
        {
            Kind = kind;
            Cycle = cycle;
            Node = node;
            Topology = topology;
            HasPayload = hasPayload;
            Descriptor = descriptor;
            Payload = payload;
        }

        /// <summary>Topology goes through the ring too, so subscribers already attached when the run starts
        /// receive it in order with the rest of the stream; the sink separately keeps a copy to replay to
        /// whoever attaches later.</summary>
        public static Queued ForTopology(EgressTopology topology) =>
            new(EgressStreamKind.Topology, null, null, false, default, default, topology);

        public static Queued ForCycle(PipelineExecutionProgress cycle) =>
            new(EgressStreamKind.Cycle, cycle, null, false, default, default);

        public static Queued ForNode(NodeExecutionEvent node) =>
            new(EgressStreamKind.NodeTransition, null, node, false, default, default);

        public static Queued ForNodeFrame(NodeExecutionEvent node, ReadOnlyMemory<byte> descriptor, ReadOnlyMemory<byte> payload) =>
            new(EgressStreamKind.NodeTransition, null, node, true, descriptor, payload);

        public EgressStreamKind Kind { get; }

        public PipelineExecutionProgress? Cycle { get; }

        public NodeExecutionEvent? Node { get; }

        public EgressTopology? Topology { get; }

        public bool HasPayload { get; }

        public ReadOnlyMemory<byte> Descriptor { get; }

        public ReadOnlyMemory<byte> Payload { get; }
    }
}
