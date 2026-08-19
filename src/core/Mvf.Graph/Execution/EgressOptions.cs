namespace Mvf.Graph.Execution;

/// <summary>Which wire transport an egress sink uses. All three carry the same framed binary records;
/// the choice is about who can consume them (a browser needs <see cref="WebSocket"/>).</summary>
public enum EgressTransport
{
    /// <summary>TCP server; native / Node consumers connect and receive the bulk stream.</summary>
    Tcp = 0,

    /// <summary>WebSocket server; the browser path (a browser cannot open a raw UDP/TCP socket).</summary>
    WebSocket = 1,

    /// <summary>UDP multicast; small state records fanned out on the LAN (no frame-data — exceeds MTU).</summary>
    Udp = 2,
}

/// <summary>Which stream kinds an egress sink emits.</summary>
[Flags]
public enum EgressStreams
{
    None = 0,

    /// <summary>Cycle + node-transition <b>state</b> (no frame payload).</summary>
    State = 1,

    /// <summary>Node-transition <b>frame-data</b> (the emitted frame bytes + descriptor).</summary>
    Frame = 2,

    All = State | Frame,
}

/// <summary>
/// Declarative egress configuration. Lives in <b>core</b> on purpose — the enable/transport/port toggle
/// is part of the run contract, not a CLI detail, so a future non-CLI host reads the same shape. The CLI
/// parses this from flags and builds the matching <see cref="IEgressSink"/>; the engine only ever touches
/// the sink. Egress is optional, non-blocking, best-effort (the Telemetry Rule) and leaves the machine, so
/// its socket implementation lives behind the sink seam in a separate project the core never references.
/// </summary>
public sealed record EgressOptions
{
    /// <summary>The conventional default egress port (state stream).</summary>
    public const int DefaultPort = 8791;

    public bool Enabled { get; init; }

    public EgressTransport Transport { get; init; } = EgressTransport.Tcp;

    public int Port { get; init; } = DefaultPort;

    public EgressStreams Streams { get; init; } = EgressStreams.State;
}
