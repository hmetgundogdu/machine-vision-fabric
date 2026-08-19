using System.Net;
using System.Net.NetworkInformation;
using System.Net.Sockets;

namespace Mvf.Egress.Client;

/// <summary>
/// Works out how — or whether — a discovered edge can actually be reached.
///
/// <para>Discovery and connectivity are not the same question. The alive-beacon goes out by multicast, so
/// every edge on the subnet is <i>discoverable</i>, including ones whose stream listens on loopback and is
/// reachable only from the machine that runs it. Resolving that difference here, once, keeps every consumer
/// from re-deriving it — and turns "connection refused" into a sentence that says what to change.</para>
/// </summary>
public static class EgressEndpointResolver
{
    /// <summary>
    /// Decides the host to connect to for <paramref name="beacon"/>.
    /// Returns false with a human-readable <paramref name="reason"/> when the edge cannot be reached from here.
    /// </summary>
    public static bool TryResolveHost(EgressBeaconInfo beacon, out string host, out string? reason)
    {
        ArgumentNullException.ThrowIfNull(beacon);

        if (!beacon.IsLoopbackOnly)
        {
            host = beacon.Address ?? "127.0.0.1";
            reason = null;
            return true;
        }

        // Loopback-bound: only this machine can attach, and only via 127.0.0.1 — the beacon's source
        // address is a real interface even for a stream that does not listen on one.
        if (beacon.Address is null || IsLocalAddress(beacon.Address))
        {
            host = "127.0.0.1";
            reason = null;
            return true;
        }

        host = string.Empty;
        reason = $"{beacon.Pipeline} listens on loopback only; " +
                 "restart it with --egress-bind 0.0.0.0 to allow watching from another machine";
        return false;
    }

    /// <summary>True when the address belongs to this host.</summary>
    private static bool IsLocalAddress(string address)
    {
        if (!IPAddress.TryParse(address, out var parsed))
        {
            return false;
        }

        if (IPAddress.IsLoopback(parsed))
        {
            return true;
        }

        try
        {
            return NetworkInterface.GetAllNetworkInterfaces()
                .SelectMany(nic => nic.GetIPProperties().UnicastAddresses)
                .Any(ua => ua.Address.Equals(parsed));
        }
        catch (NetworkInformationException)
        {
            return false; // cannot enumerate — treat as not-local rather than claiming reachability
        }
        catch (SocketException)
        {
            return false;
        }
    }
}
