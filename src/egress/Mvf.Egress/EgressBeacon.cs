using System.Net;
using System.Net.Sockets;
using System.Text.Json;
using System.Text.Json.Serialization;

namespace Mvf.Egress;

/// <summary>What a streaming edge announces on the LAN so a viewer can discover it without prior config.</summary>
public sealed record EgressBeaconInfo
{
    [JsonPropertyName("mvf")]
    public string Marker { get; init; } = EgressBeacon.Marker;

    [JsonPropertyName("v")]
    public int Version { get; init; } = 1;

    [JsonPropertyName("edgeId")]
    public string EdgeId { get; init; } = string.Empty;

    [JsonPropertyName("pipeline")]
    public string Pipeline { get; init; } = string.Empty;

    [JsonPropertyName("transport")]
    public string Transport { get; init; } = "tcp";

    [JsonPropertyName("port")]
    public int Port { get; init; }

    [JsonPropertyName("streams")]
    public string Streams { get; init; } = "state";

    [JsonPropertyName("status")]
    public string Status { get; init; } = "running";

    /// <summary>
    /// The interface the stream is actually served on ("127.0.0.1" by default, "0.0.0.0" when opened up).
    ///
    /// <para>Without this a consumer cannot tell a reachable edge from an unreachable one. The beacon
    /// travels by multicast, so it leaves over a real interface and arrives carrying that LAN source
    /// address even when the stream itself only listens on loopback — following the source address then
    /// gets a connection refused, and the failure looks like a bug rather than a configuration choice.</para>
    /// </summary>
    [JsonPropertyName("bind")]
    public string Bind { get; init; } = "127.0.0.1";

    /// <summary>
    /// Where the beacon came from, filled in by the receiver — deliberately <b>not</b> on the wire.
    ///
    /// <para>A host cannot reliably name its own reachable address: with several NICs, a container bridge
    /// or NAT in between, whatever it picks is a guess, and a wrong guess sends viewers to an address that
    /// answers for someone else. The datagram's source address is the one that demonstrably carried this
    /// packet to us, so the receiver is the only party that knows it.</para>
    ///
    /// <para>Null when the beacon was parsed without a source (e.g. straight from bytes in a test).</para>
    /// </summary>
    [JsonIgnore]
    public string? Address { get; init; }

    /// <summary>True when the stream listens on loopback only — reachable from its own host, nobody else.</summary>
    [JsonIgnore]
    public bool IsLoopbackOnly =>
        Bind is "127.0.0.1" or "localhost" or "::1" || Bind.Length == 0;

    /// <summary>The endpoint a consumer connects to, once <see cref="Address"/> is known.</summary>
    public string Endpoint => IsLoopbackOnly ? $"127.0.0.1:{Port}" : $"{Address ?? "127.0.0.1"}:{Port}";
}

/// <summary>
/// Periodically announces an active egress stream on a UDP multicast group so viewers auto-discover it.
/// Best-effort and off any hot path (its own timer thread). A viewer keeps a self-expiring list keyed by
/// <see cref="EgressBeaconInfo.EdgeId"/> + port; when the beacons stop, the edge drops off — the "alive"
/// semantics. Hand-rolled and minimal (not mDNS), TTL 1 so it stays on the local subnet.
/// </summary>
public sealed class EgressBeacon : IDisposable
{
    /// <summary>Marker value distinguishing our datagrams from anything else on the group.</summary>
    public const string Marker = "egress-beacon";

    public const string MulticastGroup = "239.255.7.71";

    public const int DiscoveryPort = 8790;

    private static readonly JsonSerializerOptions Json = new(JsonSerializerDefaults.Web);

    private readonly UdpClient _udp;
    private readonly IPEndPoint _endpoint;
    private readonly Timer _timer;
    private readonly byte[] _payload;

    /// <param name="target">Where to announce. Defaults to the multicast discovery endpoint; a test can
    /// point it at a unicast endpoint to exercise the send/serialize path deterministically.</param>
    public EgressBeacon(EgressBeaconInfo info, TimeSpan? interval = null, IPEndPoint? target = null)
    {
        ArgumentNullException.ThrowIfNull(info);
        _endpoint = target ?? new IPEndPoint(IPAddress.Parse(MulticastGroup), DiscoveryPort);
        _udp = new UdpClient(AddressFamily.InterNetwork) { Ttl = 1 };
        _payload = JsonSerializer.SerializeToUtf8Bytes(info, Json);

        var period = interval ?? TimeSpan.FromSeconds(1);
        _timer = new Timer(_ => TrySend(), null, TimeSpan.Zero, period);
    }

    private void TrySend()
    {
        try
        {
            _udp.Send(_payload, _payload.Length, _endpoint);
        }
        catch
        {
            // best effort — a missing NIC or a firewall must never fault the run
        }
    }

    /// <summary>Parses a received datagram, returning false for anything that is not one of our beacons.
    /// The marker must be <b>present</b> on the wire — we do not rely on a deserialized default.</summary>
    public static bool TryParse(ReadOnlySpan<byte> datagram, out EgressBeaconInfo info)
    {
        info = new EgressBeaconInfo();
        try
        {
            using var document = JsonDocument.Parse(datagram.ToArray());
            if (!document.RootElement.TryGetProperty("mvf", out var marker)
                || marker.ValueKind != JsonValueKind.String
                || marker.GetString() != Marker)
            {
                return false;
            }

            var parsed = JsonSerializer.Deserialize<EgressBeaconInfo>(datagram, Json);
            if (parsed is null)
            {
                return false;
            }

            info = parsed;
            return true;
        }
        catch (JsonException)
        {
            return false;
        }
    }

    public void Dispose()
    {
        _timer.Dispose();
        _udp.Dispose();
    }
}
