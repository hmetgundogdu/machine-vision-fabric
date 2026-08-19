using System.Buffers;
using System.Buffers.Binary;
using System.Text;
using Mvf.Abstractions;
using Mvf.Graph.Execution;

namespace Mvf.Egress;

/// <summary>The two stream kinds carried on the wire (see <c>docs/realtime-egress-design.md</c>).</summary>
public enum EgressStreamKind : byte
{
    Cycle = 0,
    NodeTransition = 1,

    /// <summary>The graph's shape, sent once per run and replayed to each new subscriber (wire v2+).</summary>
    Topology = 2,
}

/// <summary>
/// The framed little-endian binary codec for realtime egress. One record type, length-prefixed, two
/// stream kinds. The producer (<see cref="TcpServerEgressSink"/>) encodes; consumers (SDK / tests) decode.
/// Frame-data (the <c>PayloadDescriptor</c> + bytes) attaches at <c>hasPayload = 1</c> in a later slice;
/// state records set it to 0.
/// </summary>
public static class EgressWire
{
    /// <summary>'EG' — the record magic (little-endian u16).</summary>
    public const ushort Magic = 0x4745;

    /// <summary>
    /// The version this encoder emits. v2 added <see cref="EgressStreamKind.Topology"/>.
    ///
    /// <para>Adding a stream kind is not backwards compatible in the direction that matters: a v1 decoder
    /// reads any kind it does not know as a <see cref="EgressStreamKind.Cycle"/> record and would report
    /// confident nonsense. Bumping the version instead makes an old consumer fail loudly on the first
    /// record. <see cref="MinSupportedVersion"/> keeps the reverse direction working — a new consumer
    /// still reads a v1 producer, it just never receives topology and falls back to the node table.</para>
    /// </summary>
    public const byte Version = 2;

    /// <summary>The oldest producer version this decoder accepts.</summary>
    public const byte MinSupportedVersion = 1;

    // ---- encode ---------------------------------------------------------------------------------

    /// <summary>Encodes the graph's shape. Cycle index is 0 — topology precedes the first cycle.</summary>
    public static byte[] EncodeTopology(EgressTopology topology, string runId, uint seq)
    {
        ArgumentNullException.ThrowIfNull(topology);

        var body = new ArrayBufferWriter<byte>(1024);
        WriteHeader(body, EgressStreamKind.Topology, runId, 0, seq);
        WriteString(body, topology.Name);

        WriteU32(body, (uint)topology.Nodes.Count);
        foreach (var n in topology.Nodes)
        {
            WriteString(body, n.Id);
            WriteString(body, n.DisplayName);
            WriteString(body, n.Kind);
            WriteString(body, n.Category);
            WriteString(body, n.ModuleId ?? string.Empty);
            WriteString(body, n.PrimitiveType ?? string.Empty);
            WriteString(body, n.BuiltinType ?? string.Empty);
            WritePorts(body, n.Inputs);
            WritePorts(body, n.Outputs);
        }

        WriteU32(body, (uint)topology.Edges.Count);
        foreach (var e in topology.Edges)
        {
            WriteString(body, e.Id);
            WriteString(body, e.Kind);
            WriteString(body, e.FromNode);
            WriteString(body, e.FromPort);
            WriteString(body, e.ToNode);
            WriteString(body, e.ToPort);
        }

        return Frame(body.WrittenSpan);
    }

    private static void WritePorts(IBufferWriter<byte> w, IReadOnlyList<EgressTopologyPort> ports)
    {
        WriteU32(w, (uint)ports.Count);
        foreach (var p in ports)
        {
            WriteString(w, p.Name);
            WriteString(w, p.Channel);
            WriteString(w, p.DataType);
        }
    }

    public static byte[] EncodeCycle(PipelineExecutionProgress p, uint seq)
    {
        var body = new ArrayBufferWriter<byte>(48);
        WriteHeader(body, EgressStreamKind.Cycle, p.RunId, (uint)Math.Max(0, p.CycleIndex), seq);
        WriteU32(body, (uint)Math.Max(0, p.TotalCycles));
        WriteU32(body, (uint)Math.Max(0, p.AcceptedCycles));
        WriteU8(body, (byte)(p.CycleAccepted ? 1 : 0));
        WriteI64(body, (long)p.Elapsed.TotalMilliseconds);
        return Frame(body.WrittenSpan);
    }

    public static byte[] EncodeNodeTransition(NodeExecutionEvent e, uint seq)
    {
        var body = new ArrayBufferWriter<byte>(96);
        WriteHeader(body, EgressStreamKind.NodeTransition, e.RunId, (uint)Math.Max(0, e.CycleIndex), seq);
        WriteString(body, e.NodeId);
        WriteString(body, e.OutputPortNames.Count > 0 ? e.OutputPortNames[0] : string.Empty);
        WriteI64(body, e.DurationMicros);
        byte flags = 0;
        if (e.HasOutput) flags |= 0x01;
        if (e.Faulted) flags |= 0x02;
        WriteU8(body, flags);
        WriteI64(body, e.OutputFrameBytes ?? -1);
        WriteU8(body, 0); // hasPayload = 0 (state only)
        return Frame(body.WrittenSpan);
    }

    public static byte[] EncodeNodeTransitionFrame(
        NodeExecutionEvent e, uint seq, ReadOnlySpan<byte> payloadDescriptor, ReadOnlySpan<byte> payload)
    {
        var body = new ArrayBufferWriter<byte>(256 + payload.Length);
        WriteHeader(body, EgressStreamKind.NodeTransition, e.RunId, (uint)Math.Max(0, e.CycleIndex), seq);
        WriteString(body, e.NodeId);
        WriteString(body, e.OutputPortNames.Count > 0 ? e.OutputPortNames[0] : string.Empty);
        WriteI64(body, e.DurationMicros);
        byte flags = 0;
        if (e.HasOutput) flags |= 0x01;
        if (e.Faulted) flags |= 0x02;
        WriteU8(body, flags);
        WriteI64(body, e.OutputFrameBytes ?? payload.Length);
        WriteU8(body, 1); // hasPayload
        WriteBytes(body, payloadDescriptor);
        WriteBytes(body, payload);
        return Frame(body.WrittenSpan);
    }

    // ---- decode ---------------------------------------------------------------------------------

    public static DecodedEgressRecord Decode(ReadOnlySpan<byte> body)
    {
        var o = 0;
        var magic = ReadU16(body, ref o);
        if (magic != Magic)
        {
            throw new InvalidDataException($"Bad egress magic 0x{magic:X4} (expected 0x{Magic:X4}).");
        }

        var version = body[o++];
        if (version < MinSupportedVersion || version > Version)
        {
            throw new InvalidDataException(
                $"Unsupported egress version {version} (this build reads {MinSupportedVersion}..{Version}).");
        }

        var kind = (EgressStreamKind)body[o++];
        var runId = new Guid(body.Slice(o, 16));
        o += 16;
        var cycleIndex = ReadU32(body, ref o);
        var seq = ReadU32(body, ref o);

        var record = new DecodedEgressRecord
        {
            Kind = kind,
            RunId = runId,
            CycleIndex = cycleIndex,
            Seq = seq,
            // Body plus the 4-byte length prefix — what this record cost on the wire. Exposed because any
            // consumer measuring whether it can keep up needs bytes, and only the decoder knows them.
            WireBytes = body.Length + 4,
        };

        if (kind == EgressStreamKind.Topology)
        {
            // The run id lives in the record header, not the body — restore it so a decoded topology is
            // self-describing on its own, the way the producer built it.
            record.Topology = DecodeTopology(body, ref o) with { RunId = runId.ToString("N") };
        }
        else if (kind == EgressStreamKind.NodeTransition)
        {
            record.NodeId = ReadString(body, ref o);
            record.Port = ReadString(body, ref o);
            record.DurationMicros = ReadI64(body, ref o);
            var flags = body[o++];
            record.HasOutput = (flags & 0x01) != 0;
            record.Faulted = (flags & 0x02) != 0;
            record.OutputFrameBytes = ReadI64(body, ref o);
            record.HasPayload = body[o++] != 0;
            if (record.HasPayload)
            {
                var descriptorBytes = body.Slice(o, PayloadDescriptor.HeaderSize).ToArray();
                o += PayloadDescriptor.HeaderSize;
                record.PayloadDescriptor = descriptorBytes;
                if (PayloadDescriptor.TryReadHeader(descriptorBytes, out var descriptor))
                {
                    record.MediaType = descriptor.MediaType;
                    record.ElementType = descriptor.ElementType;
                    record.PayloadShape = descriptor.Shape;
                }

                record.Payload = body[o..].ToArray();
                o += record.Payload.Length;
            }
        }
        else
        {
            record.TotalCycles = ReadU32(body, ref o);
            record.AcceptedCycles = ReadU32(body, ref o);
            record.Accepted = body[o++] != 0;
            record.ElapsedMillis = ReadI64(body, ref o);
        }

        return record;
    }

    private static EgressTopology DecodeTopology(ReadOnlySpan<byte> body, ref int o)
    {
        var name = ReadString(body, ref o);

        var nodeCount = (int)ReadU32(body, ref o);
        var nodes = new List<EgressTopologyNode>(nodeCount);
        for (var i = 0; i < nodeCount; i++)
        {
            var id = ReadString(body, ref o);
            var displayName = ReadString(body, ref o);
            var kind = ReadString(body, ref o);
            var category = ReadString(body, ref o);
            var moduleId = ReadString(body, ref o);
            var primitiveType = ReadString(body, ref o);
            var builtinType = ReadString(body, ref o);
            var inputs = ReadPorts(body, ref o);
            var outputs = ReadPorts(body, ref o);

            nodes.Add(new EgressTopologyNode
            {
                Id = id,
                DisplayName = displayName,
                Kind = kind,
                Category = category,
                // The three type slots are mutually exclusive; empty means "not this kind", not "".
                ModuleId = NullIfEmpty(moduleId),
                PrimitiveType = NullIfEmpty(primitiveType),
                BuiltinType = NullIfEmpty(builtinType),
                Inputs = inputs,
                Outputs = outputs,
            });
        }

        var edgeCount = (int)ReadU32(body, ref o);
        var edges = new List<EgressTopologyEdge>(edgeCount);
        for (var i = 0; i < edgeCount; i++)
        {
            edges.Add(new EgressTopologyEdge
            {
                Id = ReadString(body, ref o),
                Kind = ReadString(body, ref o),
                FromNode = ReadString(body, ref o),
                FromPort = ReadString(body, ref o),
                ToNode = ReadString(body, ref o),
                ToPort = ReadString(body, ref o),
            });
        }

        return new EgressTopology { Name = name, Nodes = nodes, Edges = edges };
    }

    private static IReadOnlyList<EgressTopologyPort> ReadPorts(ReadOnlySpan<byte> b, ref int o)
    {
        var count = (int)ReadU32(b, ref o);
        var ports = new List<EgressTopologyPort>(count);
        for (var i = 0; i < count; i++)
        {
            ports.Add(new EgressTopologyPort
            {
                Name = ReadString(b, ref o),
                Channel = ReadString(b, ref o),
                DataType = ReadString(b, ref o),
            });
        }

        return ports;
    }

    private static string? NullIfEmpty(string s) => s.Length == 0 ? null : s;

    // ---- primitives -----------------------------------------------------------------------------

    private static void WriteHeader(IBufferWriter<byte> w, EgressStreamKind kind, string runId, uint cycle, uint seq)
    {
        WriteU16(w, Magic);
        WriteU8(w, Version);
        WriteU8(w, (byte)kind);
        Span<byte> guid = stackalloc byte[16];
        if (!Guid.TryParseExact(runId, "N", out var g))
        {
            g = Guid.Empty;
        }

        g.TryWriteBytes(guid);
        WriteBytes(w, guid);
        WriteU32(w, cycle);
        WriteU32(w, seq);
    }

    private static byte[] Frame(ReadOnlySpan<byte> body)
    {
        var buffer = new byte[4 + body.Length];
        BinaryPrimitives.WriteUInt32LittleEndian(buffer, (uint)body.Length);
        body.CopyTo(buffer.AsSpan(4));
        return buffer;
    }

    private static void WriteU8(IBufferWriter<byte> w, byte v)
    {
        var s = w.GetSpan(1);
        s[0] = v;
        w.Advance(1);
    }

    private static void WriteU16(IBufferWriter<byte> w, ushort v)
    {
        var s = w.GetSpan(2);
        BinaryPrimitives.WriteUInt16LittleEndian(s, v);
        w.Advance(2);
    }

    private static void WriteU32(IBufferWriter<byte> w, uint v)
    {
        var s = w.GetSpan(4);
        BinaryPrimitives.WriteUInt32LittleEndian(s, v);
        w.Advance(4);
    }

    private static void WriteI64(IBufferWriter<byte> w, long v)
    {
        var s = w.GetSpan(8);
        BinaryPrimitives.WriteInt64LittleEndian(s, v);
        w.Advance(8);
    }

    private static void WriteBytes(IBufferWriter<byte> w, ReadOnlySpan<byte> b)
    {
        var s = w.GetSpan(b.Length);
        b.CopyTo(s);
        w.Advance(b.Length);
    }

    private static void WriteString(IBufferWriter<byte> w, string value)
    {
        var bytes = Encoding.UTF8.GetBytes(value);
        WriteU16(w, (ushort)bytes.Length);
        WriteBytes(w, bytes);
    }

    private static ushort ReadU16(ReadOnlySpan<byte> b, ref int o)
    {
        var v = BinaryPrimitives.ReadUInt16LittleEndian(b.Slice(o, 2));
        o += 2;
        return v;
    }

    private static uint ReadU32(ReadOnlySpan<byte> b, ref int o)
    {
        var v = BinaryPrimitives.ReadUInt32LittleEndian(b.Slice(o, 4));
        o += 4;
        return v;
    }

    private static long ReadI64(ReadOnlySpan<byte> b, ref int o)
    {
        var v = BinaryPrimitives.ReadInt64LittleEndian(b.Slice(o, 8));
        o += 8;
        return v;
    }

    private static string ReadString(ReadOnlySpan<byte> b, ref int o)
    {
        var len = ReadU16(b, ref o);
        var s = Encoding.UTF8.GetString(b.Slice(o, len));
        o += len;
        return s;
    }
}

/// <summary>A decoded egress record. Union of the two stream kinds — read <see cref="Kind"/> first.</summary>
public sealed class DecodedEgressRecord
{
    public EgressStreamKind Kind { get; init; }

    public Guid RunId { get; init; }

    public uint CycleIndex { get; init; }

    public uint Seq { get; init; }

    /// <summary>This record's size on the wire, including the 4-byte length prefix.</summary>
    public int WireBytes { get; init; }

    // NodeTransition
    public string NodeId { get; set; } = string.Empty;

    public string Port { get; set; } = string.Empty;

    public long DurationMicros { get; set; }

    public bool HasOutput { get; set; }

    public bool Faulted { get; set; }

    public long OutputFrameBytes { get; set; }

    public bool HasPayload { get; set; }

    /// <summary>The raw 192-byte PayloadDescriptor header when <see cref="HasPayload"/>, else null.</summary>
    public byte[]? PayloadDescriptor { get; set; }

    /// <summary>The raw frame bytes when <see cref="HasPayload"/>, else null.</summary>
    public byte[]? Payload { get; set; }

    public PayloadMediaType? MediaType { get; set; }

    public PayloadElementType? ElementType { get; set; }

    public long[]? PayloadShape { get; set; }

    // Topology
    /// <summary>The graph's shape when <see cref="Kind"/> is <see cref="EgressStreamKind.Topology"/>, else
    /// null. A v1 producer never sends one.</summary>
    public EgressTopology? Topology { get; set; }

    // Cycle
    public uint TotalCycles { get; set; }

    public uint AcceptedCycles { get; set; }

    public bool Accepted { get; set; }

    public long ElapsedMillis { get; set; }
}
