using System.Net;
using System.Net.Sockets;
using System.Net.WebSockets;
using System.Runtime.CompilerServices;

namespace Mvf.Egress.Client;

/// <summary>
/// The .NET consumer side of realtime egress: connect to an edge over TCP or UDP and decode the live
/// stream, and discover streaming edges via the alive-beacon. The wire codec is shared with the producer
/// (<c>Mvf.Egress</c>), so this never re-implements the format.
/// </summary>
public static class EgressClient
{
    /// <summary>Connects to a TCP egress server and yields records until the server closes or cancellation.</summary>
    public static async IAsyncEnumerable<DecodedEgressRecord> StreamTcpAsync(
        string host, int port, [EnumeratorCancellation] CancellationToken cancellationToken = default)
    {
        using var client = new TcpClient();
        await client.ConnectAsync(host, port, cancellationToken).ConfigureAwait(false);
        var reader = new EgressStreamReader(client.GetStream());

        while (true)
        {
            DecodedEgressRecord? record;
            try
            {
                record = await reader.ReadAsync(cancellationToken).ConfigureAwait(false);
            }
            catch (Exception ex) when (ex is IOException or OperationCanceledException or SocketException)
            {
                yield break;
            }

            if (record is null)
            {
                yield break;
            }

            yield return record;
        }
    }

    /// <summary>
    /// Connects to a WebSocket egress server and yields records until the socket closes or cancellation.
    /// Each binary message is exactly one record body — the WebSocket message boundary replaces the length
    /// prefix, so the frame is reassembled before decoding.
    /// </summary>
    public static async IAsyncEnumerable<DecodedEgressRecord> StreamWebSocketAsync(
        string host, int port, [EnumeratorCancellation] CancellationToken cancellationToken = default)
    {
        using var socket = new ClientWebSocket();
        await socket.ConnectAsync(new Uri($"ws://{host}:{port}/"), cancellationToken).ConfigureAwait(false);

        var buffer = new byte[64 * 1024];
        var message = new MemoryStream();

        while (!cancellationToken.IsCancellationRequested)
        {
            message.SetLength(0);
            WebSocketReceiveResult result;
            do
            {
                try
                {
                    result = await socket.ReceiveAsync(buffer, cancellationToken).ConfigureAwait(false);
                }
                catch (Exception ex) when (ex is WebSocketException or OperationCanceledException or ObjectDisposedException)
                {
                    yield break;
                }

                if (result.MessageType == WebSocketMessageType.Close)
                {
                    yield break;
                }

                message.Write(buffer, 0, result.Count);
            }
            while (!result.EndOfMessage);

            DecodedEgressRecord? record;
            try
            {
                record = EgressWire.Decode(message.GetBuffer().AsSpan(0, (int)message.Length));
            }
            catch (InvalidDataException)
            {
                continue; // a record this build cannot read must not end the stream
            }

            yield return record;
        }
    }

    /// <summary>Joins the UDP state multicast group and yields each datagram's record (state-only stream).</summary>
    public static async IAsyncEnumerable<DecodedEgressRecord> StreamUdpAsync(
        int port, [EnumeratorCancellation] CancellationToken cancellationToken = default)
    {
        using var udp = new UdpClient();
        ShareThePort(udp);
        udp.Client.Bind(new IPEndPoint(IPAddress.Any, port));
        udp.JoinMulticastGroup(IPAddress.Parse(UdpEgressSink.DataGroup));

        while (!cancellationToken.IsCancellationRequested)
        {
            UdpReceiveResult datagram;
            try
            {
                datagram = await udp.ReceiveAsync(cancellationToken).ConfigureAwait(false);
            }
            catch (Exception ex) when (ex is OperationCanceledException or SocketException or ObjectDisposedException)
            {
                yield break;
            }

            DecodedEgressRecord? record = null;
            try
            {
                record = EgressWire.Decode(datagram.Buffer);
            }
            catch (InvalidDataException)
            {
                // not one of our datagrams — skip
            }

            if (record is not null)
            {
                yield return record;
            }
        }
    }

    /// <summary>
    /// Lets several consumers share one multicast port — two viewers on a machine, or a viewer beside a
    /// browser relay, is the normal case, not a conflict.
    ///
    /// <para><c>ReuseAddress</c> alone is not enough on Windows: .NET sets <c>ExclusiveAddressUse</c> on a
    /// <see cref="UdpClient"/> there, and a port already held exclusively fails the second bind with
    /// WSAEACCES — reported as <b>access denied</b>, which sends everyone looking for a permissions problem
    /// that elevation cannot fix. Both flags must be cleared, and before the bind.</para>
    /// </summary>
    private static void ShareThePort(UdpClient udp)
    {
        try
        {
            udp.ExclusiveAddressUse = false;
        }
        catch (SocketException)
        {
            // Not supported on this platform/socket state — ReuseAddress below still covers the common case.
        }

        udp.Client.SetSocketOption(SocketOptionLevel.Socket, SocketOptionName.ReuseAddress, true);
    }

    /// <summary>Listens on the discovery multicast group and yields alive-beacons as they arrive.</summary>
    public static async IAsyncEnumerable<EgressBeaconInfo> DiscoverAsync(
        [EnumeratorCancellation] CancellationToken cancellationToken = default)
    {
        using var udp = new UdpClient();
        ShareThePort(udp);
        udp.Client.Bind(new IPEndPoint(IPAddress.Any, EgressBeacon.DiscoveryPort));
        udp.JoinMulticastGroup(IPAddress.Parse(EgressBeacon.MulticastGroup));

        while (!cancellationToken.IsCancellationRequested)
        {
            UdpReceiveResult datagram;
            try
            {
                datagram = await udp.ReceiveAsync(cancellationToken).ConfigureAwait(false);
            }
            catch (Exception ex) when (ex is OperationCanceledException or SocketException or ObjectDisposedException)
            {
                yield break;
            }

            if (EgressBeacon.TryParse(datagram.Buffer, out var info))
            {
                // The sender cannot name its own reachable address; the datagram's source can.
                yield return info with { Address = datagram.RemoteEndPoint.Address.ToString() };
            }
        }
    }
}
