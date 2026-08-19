using System.Diagnostics;

namespace Mvf.Egress.Client;

/// <summary>
/// A live, self-expiring registry of discovered edges, keyed by edge id + port. Feed it beacons from
/// <see cref="EgressClient.DiscoverAsync"/>; <see cref="Live"/> returns the ones still announcing and drops
/// any whose beacons have lapsed — the "alive" semantics. Mirrors the TS SDK's <c>EdgeRegistry</c>.
/// </summary>
public sealed class EgressEdgeRegistry
{
    private readonly Dictionary<string, Entry> _edges = new(StringComparer.Ordinal);
    private readonly object _lock = new();

    public void Apply(EgressBeaconInfo beacon, long? nowMs = null)
    {
        ArgumentNullException.ThrowIfNull(beacon);
        var stamp = nowMs ?? Now();
        lock (_lock)
        {
            _edges[$"{beacon.EdgeId}:{beacon.Port}"] = new Entry(beacon, stamp);
        }
    }

    public IReadOnlyList<EgressBeaconInfo> Live(long ttlMs = 3000, long? nowMs = null)
    {
        var stamp = nowMs ?? Now();
        var alive = new List<EgressBeaconInfo>();
        lock (_lock)
        {
            foreach (var (key, entry) in _edges.ToArray())
            {
                if (stamp - entry.LastSeenMs > ttlMs)
                {
                    _edges.Remove(key);
                }
                else
                {
                    alive.Add(entry.Beacon);
                }
            }
        }

        return alive;
    }

    private static long Now() => Stopwatch.GetTimestamp() * 1000 / Stopwatch.Frequency;

    private readonly record struct Entry(EgressBeaconInfo Beacon, long LastSeenMs);
}
