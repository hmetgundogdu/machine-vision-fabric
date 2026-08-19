using Mvf.Abstractions;
using Mvf.Cli.Imaging;
using Mvf.Egress;
using Mvf.Egress.Client;
using Mvf.Graph.Pipelines;
using Spectre.Console;
using Spectre.Console.Rendering;

namespace Mvf.Cli.Tui;

/// <summary>
/// The observer: discover the pipelines streaming on this network, pick one, and watch it live.
///
/// <para>Two pages. The <b>discovery</b> page is a self-expiring list built from the alive-beacon — an edge
/// that stops beaconing drops off, so the list means "streaming right now" rather than "was seen once". The
/// <b>watch</b> page attaches to one edge and renders it with the same <see cref="GraphRenderer"/> the local
/// run dashboard uses, because the topology record makes the remote graph a real graph again.</para>
///
/// <para>This is read-only by construction: the observer opens a client connection and decodes. There is no
/// path from here back into the run — edge-first execution means a watcher must never be able to steer.</para>
/// </summary>
internal sealed class WatchDashboard
{
    private const int RefreshMs = 120;

    /// <summary>How long an edge stays listed after its last beacon. Three beacons at the 1s cadence: long
    /// enough to ride out a lost datagram, short enough that a stopped pipeline disappears promptly.</summary>
    private const long EdgeTtlMs = 3000;

    private readonly EgressEdgeRegistry _registry = new();
    private readonly object _edgesGate = new();
    private IReadOnlyList<EgressBeaconInfo> _edges = [];
    private int _cursor;
    private string? _unreachable;

    /// <summary>
    /// The watch page's node cursor: an index into the graph's traversal order (or into the observed rows
    /// when there is no topology). Until an arrow key is pressed it follows the executing node, so the
    /// viewport rides along with the run; after that it belongs to the operator, and the viewport tracks
    /// it — which is what makes a graph wider than the terminal navigable at all.
    /// </summary>
    private int _nodeIndex;

    /// <summary>True once the operator has taken the node cursor over with an arrow key.</summary>
    private bool _nodeNavigated;

    public async Task RunAsync(CancellationToken cancellationToken)
    {
        using var cts = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        var discovery = Task.Run(() => DiscoverLoopAsync(cts.Token), cts.Token);

        try { Console.CursorVisible = false; } catch { /* not supported on all hosts */ }
        Console.Clear();

        try
        {
            while (!cts.IsCancellationRequested)
            {
                RefreshEdges();
                TuiPainter.PaintInPlace(BuildDiscoveryView());

                var key = ReadKey();
                switch (key)
                {
                    case ConsoleKey.UpArrow:
                        MoveCursor(-1);
                        break;
                    case ConsoleKey.DownArrow:
                        MoveCursor(+1);
                        break;
                    case ConsoleKey.Enter when SelectedEdge is { } edge:
                        // Resolve before switching pages: an edge that cannot be reached from here is a
                        // configuration fact worth stating on the list, not a blank page and a socket error.
                        if (EgressEndpointResolver.TryResolveHost(edge, out var host, out var reason))
                        {
                            _unreachable = null;
                            await WatchEdgeAsync(edge, host, cts.Token).ConfigureAwait(false);
                            Console.Clear();
                        }
                        else
                        {
                            _unreachable = reason;
                        }

                        break;
                    case ConsoleKey.Q or ConsoleKey.Escape:
                        return;
                }

                await Task.Delay(RefreshMs, cts.Token).ConfigureAwait(false);
            }
        }
        catch (OperationCanceledException) { /* clean shutdown */ }
        finally
        {
            await cts.CancelAsync().ConfigureAwait(false);
            try { await discovery.ConfigureAwait(false); } catch { /* shutting down */ }
            try { Console.CursorVisible = true; } catch { }
            Console.Clear();
        }
    }

    private EgressBeaconInfo? SelectedEdge
    {
        get
        {
            var edges = _edges;
            return edges.Count == 0 ? null : edges[Math.Clamp(_cursor, 0, edges.Count - 1)];
        }
    }

    private void MoveCursor(int delta)
    {
        var count = _edges.Count;
        if (count == 0)
        {
            _cursor = 0;
            return;
        }

        _cursor = Math.Clamp(_cursor + delta, 0, count - 1);
    }

    private void RefreshEdges()
    {
        lock (_edgesGate)
        {
            // Ordered by identity, not by recency: a list that reshuffles under the cursor is unusable
            // when the whole point is to point at one row and press enter.
            _edges = [.. _registry.Live(EdgeTtlMs).OrderBy(e => e.Pipeline, StringComparer.OrdinalIgnoreCase)
                                                  .ThenBy(e => e.Port)];
        }

        if (_cursor >= _edges.Count)
        {
            _cursor = Math.Max(0, _edges.Count - 1);
        }
    }

    private async Task DiscoverLoopAsync(CancellationToken cancellationToken)
    {
        try
        {
            await foreach (var beacon in EgressClient.DiscoverAsync(cancellationToken).ConfigureAwait(false))
            {
                lock (_edgesGate)
                {
                    _registry.Apply(beacon);
                }
            }
        }
        catch (OperationCanceledException) { }
    }

    private static ConsoleKey? ReadKeyCore()
    {
        try
        {
            return Console.KeyAvailable ? Console.ReadKey(intercept: true).Key : null;
        }
        catch
        {
            return null; // redirected input — no keyboard to read
        }
    }

    private static ConsoleKey ReadKey() => ReadKeyCore() ?? ConsoleKey.NoName;

    // ── Discovery page ────────────────────────────────────────────────────────

    private IRenderable BuildDiscoveryView()
    {
        var table = new Table()
            .Border(TableBorder.Rounded)
            .BorderColor(Color.Grey37)
            .Expand();

        table.AddColumn(new TableColumn("[grey62]  pipeline[/]").NoWrap());
        table.AddColumn(new TableColumn("[grey62]edge[/]").NoWrap());
        table.AddColumn(new TableColumn("[grey62]endpoint[/]").NoWrap());
        table.AddColumn(new TableColumn("[grey62]transport[/]").NoWrap());
        table.AddColumn(new TableColumn("[grey62]streams[/]").NoWrap());
        table.AddColumn(new TableColumn("[grey62]status[/]").NoWrap());

        var edges = _edges;
        if (edges.Count == 0)
        {
            table.AddRow("[grey42]listening...[/]", "[grey42]-[/]", "[grey42]-[/]", "[grey42]-[/]", "[grey42]-[/]", "[grey42]-[/]");
        }

        for (var i = 0; i < edges.Count; i++)
        {
            var e = edges[i];
            var selected = i == _cursor;
            var caret = selected ? $"[deepskyblue1]{Glyphs.Cursor}[/] " : "  ";
            var name = selected ? $"[bold white]{Markup.Escape(e.Pipeline)}[/]" : Markup.Escape(e.Pipeline);

            table.AddRow(
                caret + name,
                $"[grey62]{Markup.Escape(Shorten(e.EdgeId))}[/]",
                $"[grey78]{Markup.Escape(e.Endpoint)}[/]",
                $"[steelblue1]{Markup.Escape(e.Transport)}[/]",
                $"[grey62]{Markup.Escape(e.Streams)}[/]",
                StatusMarkup(e.Status));
        }

        var hint = _unreachable is { Length: > 0 } u
            ? $"[gold1]{Glyphs.Invalid} {Markup.Escape(u)}[/]"
            : $"[grey42]{Glyphs.FieldUpDown} select {Glyphs.Separator} enter watch {Glyphs.Separator} q quit[/]";
        var header = $"[deepskyblue1]mvf watch[/] [grey42]{Glyphs.Separator}[/] discovery " +
                     $"[grey42]{Glyphs.Separator} beacon {EgressBeacon.MulticastGroup}:{EgressBeacon.DiscoveryPort} " +
                     $"{Glyphs.Separator} {edges.Count} live[/]";

        return new Rows(
            new Markup(header),
            new Text(string.Empty),
            table,
            new Markup(hint));
    }

    private static string StatusMarkup(string status) => status.ToLowerInvariant() switch
    {
        "running" => "[green3]running[/]",
        "paused" => "[gold1]paused[/]",
        _ => $"[grey62]{Markup.Escape(status)}[/]",
    };

    private static string Shorten(string value) => value.Length <= 8 ? value : value[..8];

    // ── Watch page ────────────────────────────────────────────────────────────

    private async Task WatchEdgeAsync(EgressBeaconInfo edge, string host, CancellationToken cancellationToken)
    {
        using var cts = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        var session = new WatchSession();
        string? error = null;

        var consume = Task.Run(async () =>
        {
            try
            {
                await foreach (var record in StreamAsync(edge, host, cts.Token).ConfigureAwait(false))
                {
                    session.Apply(record, record.WireBytes);
                }
            }
            catch (OperationCanceledException) { }
            catch (Exception ex)
            {
                // Surfaced on the page rather than thrown: the observer failing to attach is information
                // about that edge, not a reason to take the whole watcher down.
                error = ex.Message;
            }
        }, cts.Token);

        Console.Clear();
        _nodeIndex = 0;
        _nodeNavigated = false;

        try
        {
            while (!cts.IsCancellationRequested)
            {
                SyncNodeCursorToActive(session);
                TuiPainter.PaintInPlace(BuildWatchView(edge, session, error));

                var key = ReadKeyCore();
                switch (key)
                {
                    case ConsoleKey.LeftArrow:  MoveNodeCursor(session, -1); break;
                    case ConsoleKey.RightArrow: MoveNodeCursor(session, +1); break;
                    case ConsoleKey.Tab:        MoveNodeCursor(session, +1); break;
                    case ConsoleKey.UpArrow:    MoveNodeCursorWithinLayer(session, -1); break;
                    case ConsoleKey.DownArrow:  MoveNodeCursorWithinLayer(session, +1); break;

                    case ConsoleKey.Enter when SelectedNodeId(session) is { } nodeId:
                        await ShowNodeDetailAsync(session, nodeId, cts.Token).ConfigureAwait(false);
                        Console.Clear();
                        break;

                    case ConsoleKey.Q or ConsoleKey.Escape or ConsoleKey.Backspace:
                        return;
                }

                await Task.Delay(RefreshMs, cts.Token).ConfigureAwait(false);
            }
        }
        catch (OperationCanceledException) { }
        finally
        {
            await cts.CancelAsync().ConfigureAwait(false);
            try { await consume.ConfigureAwait(false); } catch { /* shutting down */ }
        }
    }

    // ── Node cursor ───────────────────────────────────────────────────────────

    /// <summary>
    /// The order the cursor walks. With topology it is the graph's own left-to-right traversal, so moving
    /// the cursor moves the viewport along the pipeline; without it, the observed rows are all there is.
    /// </summary>
    private static IReadOnlyList<string> NodeOrder(WatchSession session) =>
        session.Layout is { } layout
            ? layout.TraversalOrder
            : [.. session.ObservedNodes.Select(n => n.NodeId)];

    private static string? SelectedNodeIdIn(IReadOnlyList<string> order, int index) =>
        order.Count == 0 ? null : order[Math.Clamp(index, 0, order.Count - 1)];

    private string? SelectedNodeId(WatchSession session) => SelectedNodeIdIn(NodeOrder(session), _nodeIndex);

    /// <summary>Steps through the flat left-to-right order, wrapping at both ends.</summary>
    private void MoveNodeCursor(WatchSession session, int delta)
    {
        var order = NodeOrder(session);
        if (order.Count == 0)
        {
            return;
        }

        _nodeNavigated = true;
        _nodeIndex = (_nodeIndex + delta % order.Count + order.Count) % order.Count;
    }

    /// <summary>Moves within the current layer — the graph's vertical axis, where parallel branches sit.</summary>
    private void MoveNodeCursorWithinLayer(WatchSession session, int delta)
    {
        if (session.Layout is not { } layout || SelectedNodeId(session) is not { } current)
        {
            return;
        }

        if (!layout.NodePositions.TryGetValue(current, out var pos))
        {
            return;
        }

        var layer = layout.Layers[pos.Layer];
        var target = pos.Slot + delta;
        if (target < 0 || target >= layer.Count)
        {
            return;
        }

        _nodeNavigated = true;
        var index = layout.TraversalOrder.ToList().IndexOf(layer[target]);
        if (index >= 0)
        {
            _nodeIndex = index;
        }
    }

    /// <summary>Keeps the cursor riding the executing node until the operator takes over.</summary>
    private void SyncNodeCursorToActive(WatchSession session)
    {
        if (_nodeNavigated || session.State?.LastActiveNodeId is not { } active)
        {
            return;
        }

        var index = NodeOrder(session).ToList().IndexOf(active);
        if (index >= 0)
        {
            _nodeIndex = index;
        }
    }

    private static IAsyncEnumerable<DecodedEgressRecord> StreamAsync(
        EgressBeaconInfo edge, string host, CancellationToken cancellationToken)
    {
        return edge.Transport.ToLowerInvariant() switch
        {
            "udp" => EgressClient.StreamUdpAsync(edge.Port, cancellationToken),
            "ws" or "websocket" => EgressClient.StreamWebSocketAsync(host, edge.Port, cancellationToken),
            _ => EgressClient.StreamTcpAsync(host, edge.Port, cancellationToken),
        };
    }

    private IRenderable BuildWatchView(EgressBeaconInfo edge, WatchSession session, string? error)
    {
        var width = SafeWidth();
        var selected = SelectedNodeId(session);
        var rows = new List<IRenderable>
        {
            new Markup(BuildWatchHeader(edge, session)),
            new Text(string.Empty),
        };

        if (error is { Length: > 0 })
        {
            rows.Add(new Markup($"[red1]cannot attach:[/] [grey78]{Markup.Escape(error)}[/]"));
            rows.Add(new Text(string.Empty));
        }

        // With topology the remote run draws exactly like a local one; without it (a v1 producer, or the
        // first moments of a UDP stream) the observed-node table carries the run instead.
        if (session.Layout is { } layout && session.State is { } state)
        {
            rows.Add(GraphRenderer.Render(
                layout,
                state.Nodes,
                new Dictionary<string, GraphRenderer.NodeConfigLine>(StringComparer.OrdinalIgnoreCase),
                selectedNodeId: selected,
                activeNodeId: state.LastActiveNodeId,
                terminalWidth: width,
                // Anchor on the *selected* node, not the executing one: that is what lets the operator
                // walk a graph wider than the terminal instead of being pinned to wherever the run is.
                anchorLayer: selected is { } id ? layout.LayerOf(id) : 0,
                nowMs: Environment.TickCount64,
                animate: true));
            rows.Add(new Text(string.Empty));
        }
        else
        {
            rows.Add(new Markup("[grey42]no topology yet - showing observed nodes " +
                                "(an older engine never sends one)[/]"));
            rows.Add(new Text(string.Empty));
        }

        rows.Add(BuildNodeTable(session, selected));

        if (session.LastFrame is { } frame)
        {
            rows.Add(new Text(string.Empty));
            rows.Add(new Markup(BuildFrameLine(frame)));
        }

        rows.Add(new Markup(
            $"[grey42]{Glyphs.MoveHint} move {Glyphs.Separator} {Glyphs.FieldUpDown} within layer " +
            $"{Glyphs.Separator} enter node detail {Glyphs.Separator} q back to discovery[/]"));
        return new Rows(rows);
    }

    private static string BuildWatchHeader(EgressBeaconInfo edge, WatchSession session)
    {
        var name = session.PipelineName is { Length: > 0 } n ? n : edge.Pipeline;
        var run = session.RunId is { Length: >= 8 } r ? r[..8] : session.RunId;
        var attached = session.Records > 0;

        var live = attached
            ? $"[green3]attached[/] [grey42]{Glyphs.Separator}[/] " +
              $"cyc:[bold]{session.LastCycle}[/] ok:[green3]{session.AcceptedCycles}[/] " +
              $"[grey42]{Glyphs.Separator}[/] {FormatRate(session)}"
            : "[gold1]waiting for records[/]";

        var gaps = session.Gaps > 0 ? $" [grey42]{Glyphs.Separator}[/] [gold1]dropped:{session.Gaps}[/]" : string.Empty;

        return $"[deepskyblue1]MVF[/] [grey42]{Glyphs.Separator}[/] [bold]{Markup.Escape(name)}[/] " +
               $"[grey42]{Markup.Escape(edge.Endpoint)} ({Markup.Escape(edge.Transport)})[/] " +
               $"[grey42]{Glyphs.Separator}[/] run:[grey78]{Markup.Escape(run)}[/] " +
               $"[grey42]{Glyphs.Separator}[/] {live}{gaps}";
    }

    /// <summary>The consumer-side throughput readout — records/s and bytes/s as actually received. This is
    /// the number that answers "can a watcher keep up with this pipeline".</summary>
    private static string FormatRate(WatchSession session) =>
        $"[grey78]{session.RecordsPerSecond:0} rec/s {Glyphs.Separator} {FormatBytes(session.BytesPerSecond)}/s[/]";

    private static string FormatBytes(double bytes) => bytes switch
    {
        >= 1024d * 1024 * 1024 => $"{bytes / (1024d * 1024 * 1024):0.00} GB",
        >= 1024d * 1024 => $"{bytes / (1024d * 1024):0.0} MB",
        >= 1024 => $"{bytes / 1024:0.0} KB",
        _ => $"{bytes:0} B",
    };

    private static IRenderable BuildNodeTable(WatchSession session, string? selectedNodeId)
    {
        var table = new Table()
            .Border(TableBorder.Rounded)
            .BorderColor(Color.Grey37)
            .Expand();

        // Deliberately few columns: every one added here is width taken from the node name, and a row that
        // wraps to two lines costs more legibility than the column bought.
        table.AddColumn(new TableColumn("[grey62]  name[/]").NoWrap());
        table.AddColumn(new TableColumn("[grey62]id[/]").NoWrap());
        table.AddColumn(new TableColumn("[grey62]port[/]").NoWrap());
        table.AddColumn(new TableColumn("[grey62]cyc[/]").RightAligned().NoWrap());
        table.AddColumn(new TableColumn("[grey62]last[/]").RightAligned().NoWrap());
        table.AddColumn(new TableColumn("[grey62]avg[/]").RightAligned().NoWrap());
        table.AddColumn(new TableColumn("[grey62]out[/]").RightAligned().NoWrap());
        table.AddColumn(new TableColumn("[grey62]state[/]").NoWrap());

        var nodes = session.ObservedNodes;
        if (nodes.Count == 0)
        {
            table.AddRow("[grey42]no node records yet[/]", "-", "-", "-", "-", "-", "-", "-");
            return table;
        }

        foreach (var n in nodes)
        {
            var state = n.LastFaulted
                ? $"[red1]{Glyphs.Faulted} fault x{n.Faults}[/]"
                : n.LastHasOutput ? $"[green3]{Glyphs.Done} ok[/]" : $"[grey62]{Glyphs.Idle} idle[/]";

            // The table and the graph share one cursor, so the row under it is marked here too — otherwise
            // pressing enter on a wide graph is a guess about which node you are on.
            var isSelected = string.Equals(n.NodeId, selectedNodeId, StringComparison.OrdinalIgnoreCase);
            var caret = isSelected ? $"[deepskyblue1]{Glyphs.Cursor}[/] " : "  ";
            var name = isSelected
                ? $"[bold white]{Markup.Escape(n.DisplayName)}[/]"
                : $"[white]{Markup.Escape(n.DisplayName)}[/]";

            table.AddRow(
                caret + name,
                $"[grey58]{Markup.Escape(n.NodeId)}[/]",
                $"[grey62]{Markup.Escape(n.LastPort is { Length: > 0 } p ? p : Glyphs.None)}[/]",
                $"[grey78]{n.Cycles}[/]",
                $"[grey78]{FormatMicros(n.LastDurationMicros)}[/]",
                $"[grey62]{FormatMicros((long)n.AverageDurationMicros)}[/]",
                $"[grey62]{(n.LastFrameBytes > 0 ? FormatBytes(n.LastFrameBytes) : Glyphs.None)}[/]",
                state);
        }

        return table;
    }

    // ── Node detail page ──────────────────────────────────────────────────────

    /// <summary>
    /// One node, up close: identity, its typed ports, live timing, and whatever its last frame was.
    ///
    /// <para>This is the drill-down the flat table cannot give. It is also the only place frame <b>bytes</b>
    /// are retained — the session keeps them for this node alone while the page is open (see
    /// <see cref="WatchSession.SetPayloadInterest"/>), so an observer never grows with the graph.</para>
    /// </summary>
    private async Task ShowNodeDetailAsync(WatchSession session, string nodeId, CancellationToken cancellationToken)
    {
        session.SetPayloadInterest(nodeId);
        string? notice = null;
        Console.Clear();

        try
        {
            while (!cancellationToken.IsCancellationRequested)
            {
                TuiPainter.PaintInPlace(BuildNodeDetail(session, nodeId, notice));

                switch (ReadKeyCore())
                {
                    case ConsoleKey.S:
                        notice = SaveLastPayload(session, nodeId);
                        break;
                    // The same back keys the run dashboard's node detail uses — left arrow reads as
                    // "out of this node" once you have been walking the graph with the arrows.
                    case ConsoleKey.Escape or ConsoleKey.Q or ConsoleKey.LeftArrow or ConsoleKey.Backspace:
                        return;
                }

                await Task.Delay(RefreshMs, cancellationToken).ConfigureAwait(false);
            }
        }
        catch (OperationCanceledException) { }
        finally
        {
            session.SetPayloadInterest(null);
        }
    }

    /// <summary>
    /// Writes the node's last frame into the working directory — as a PNG when the frame is an image.
    ///
    /// <para>The terminal is not a viewer and pretending otherwise would only work on the developer's own
    /// machine; the panel PCs this runs on cannot render inline images at all. Writing a real image file is
    /// the honest version of "let me see it" — open it with whatever the host already has.</para>
    /// </summary>
    private static string SaveLastPayload(WatchSession session, string nodeId)
    {
        var node = session.Node(nodeId);
        if (node?.LastPayload is not { Length: > 0 } payload)
        {
            return "[gold1]nothing to save yet - this node has published no frame bytes[/]";
        }

        // A frame that is really an image is written as a PNG, so the file opens in whatever the machine
        // has. Anything else is written as it came: inventing a container for bytes whose meaning we do not
        // know would only produce a file that lies about itself.
        var frame = node.LastFrame;
        byte[] bytes;
        string extension;
        if (frame is not null && IsGrayscale2D(frame))
        {
            try
            {
                bytes = GrayscalePng.Encode(payload, (int)frame.Shape![1], (int)frame.Shape![0]);
                extension = "png";
            }
            catch (ArgumentException)
            {
                bytes = payload;   // shape and bytes disagree — save the truth, not a guess
                extension = "bin";
            }
        }
        else
        {
            bytes = payload;
            extension = frame?.MediaType == PayloadMediaType.Json ? "json" : "bin";
        }

        var path = Path.GetFullPath($"mvf-frame-{nodeId}-{DateTime.Now:HHmmss}.{extension}");
        try
        {
            File.WriteAllBytes(path, bytes);
            return $"[green3]saved[/] [grey78]{Markup.Escape(path)}[/] [grey62]({FormatBytes(bytes.Length)})[/]";
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
        {
            return $"[red1]could not save:[/] [grey78]{Markup.Escape(ex.Message)}[/]";
        }
    }

    private static IRenderable BuildNodeDetail(WatchSession session, string nodeId, string? notice)
    {
        var node = session.Node(nodeId);
        var definitionNode = session.Definition?.Nodes
            .FirstOrDefault(n => string.Equals(n.Id, nodeId, StringComparison.OrdinalIgnoreCase));

        var title = new Markup(
            $"[bold deepskyblue1]{Glyphs.NodeMark} NODE[/]  [bold]{Markup.Escape(node?.DisplayName ?? nodeId)}[/]   " +
            $"[grey]id:[/][grey58]{Markup.Escape(nodeId)}[/]   " +
            $"[grey]kind:[/][grey58]{Markup.Escape(node?.Kind is { Length: > 0 } k ? k : "-")}[/]   " +
            $"[grey]type:[/][grey58]{Markup.Escape(node?.TypeLabel is { Length: > 0 } t ? t : "-")}[/]   " +
            $"[grey]cat:[/][grey58]{Markup.Escape(node?.Category is { Length: > 0 } c ? c : "-")}[/]");

        var top = new Grid();
        top.AddColumn(new GridColumn());
        top.AddColumn(new GridColumn());
        top.AddRow(BuildPortsPanel(definitionNode), BuildNodeStatsPanel(node));

        var rows = new List<IRenderable>
        {
            title,
            new Rule { Style = Style.Parse("grey23") },
            top,
            BuildPayloadPanel(node),
        };

        var controls = $"[grey42]s save image {Glyphs.Separator} esc/{Glyphs.ArrowLeft} back[/]";
        if (notice is { Length: > 0 })
        {
            controls += $"    {notice}";
        }

        rows.Add(new Markup(controls));
        return new Rows(rows);
    }

    /// <summary>The node's typed ports — the part of the contract that travels with the topology.</summary>
    private static IRenderable BuildPortsPanel(PipelineNodeDefinition? node)
    {
        var table = new Table().Border(TableBorder.None).Expand();
        table.AddColumn(new TableColumn(string.Empty).NoWrap());
        table.AddColumn(new TableColumn(string.Empty).NoWrap());
        table.AddColumn(new TableColumn(string.Empty).NoWrap());

        if (node is null)
        {
            table.AddRow("[grey42]no topology - ports unknown[/]", string.Empty, string.Empty);
        }
        else
        {
            foreach (var p in node.Inputs)
            {
                table.AddRow($"[grey62]in {Glyphs.ArrowRight}[/]", PortName(p), PortType(p));
            }

            foreach (var p in node.Outputs)
            {
                table.AddRow($"[grey62]out {Glyphs.ArrowRight}[/]", PortName(p), PortType(p));
            }

            if (node.Inputs.Count == 0 && node.Outputs.Count == 0)
            {
                table.AddRow("[grey42]portless[/]", string.Empty, string.Empty);
            }
        }

        return new Panel(table)
        {
            Header = new PanelHeader("[grey62] ports [/]"),
            Border = BoxBorder.Rounded,
            BorderStyle = Style.Parse("grey23"),
            Expand = true,
        };
    }

    private static string PortName(PipelinePortDefinition p) => $"[white]{Markup.Escape(p.Name)}[/]";

    // Colour carries the data/control split here exactly as it does on the graph edges.
    private static string PortType(PipelinePortDefinition p) =>
        p.Channel.Equals("control", StringComparison.OrdinalIgnoreCase)
            ? $"[orchid]{Markup.Escape(p.Channel)}[/] [grey58]{Markup.Escape(p.DataType)}[/]"
            : $"[steelblue1]{Markup.Escape(p.Channel)}[/] [grey58]{Markup.Escape(p.DataType)}[/]";

    private static IRenderable BuildNodeStatsPanel(WatchSession.ObservedNode? node)
    {
        var table = new Table().Border(TableBorder.None).Expand();
        table.AddColumn(new TableColumn(string.Empty).NoWrap());
        table.AddColumn(new TableColumn(string.Empty).NoWrap());

        if (node is null)
        {
            table.AddRow("[grey42]nothing observed yet[/]", string.Empty);
        }
        else
        {
            table.AddRow("[grey62]cycles seen[/]", $"[white]{node.Cycles}[/]");
            table.AddRow("[grey62]last[/]", $"[grey78]{FormatMicros(node.LastDurationMicros)}[/]");
            table.AddRow("[grey62]average[/]", $"[grey78]{FormatMicros((long)node.AverageDurationMicros)}[/]");
            table.AddRow("[grey62]faults[/]", node.Faults > 0
                ? $"[red1]{node.Faults}[/]"
                : "[green3]0[/]");
            table.AddRow("[grey62]last output[/]", node.LastHasOutput
                ? $"[green3]{Glyphs.Done} {Markup.Escape(node.LastPort is { Length: > 0 } p ? p : "-")}[/]"
                : "[grey62]none[/]");
            table.AddRow("[grey62]last frame[/]", node.LastFrameBytes > 0
                ? $"[grey78]{FormatBytes(node.LastFrameBytes)}[/]"
                : $"[grey62]{Glyphs.None}[/]");
        }

        return new Panel(table)
        {
            Header = new PanelHeader("[grey62] observed [/]"),
            Border = BoxBorder.Rounded,
            BorderStyle = Style.Parse("grey23"),
            Expand = true,
        };
    }

    private static IRenderable BuildPayloadPanel(WatchSession.ObservedNode? node)
    {
        var body = BuildPayloadBody(node);
        return new Panel(new Markup(body))
        {
            Header = new PanelHeader("[grey62] last frame [/]"),
            Border = BoxBorder.Rounded,
            BorderStyle = Style.Parse("grey23"),
            Expand = true,
        };
    }

    /// <summary>
    /// Renders what the last frame actually is, per media type: JSON is text and gets shown as text; a
    /// 2-D uint8 payload gets a coarse ASCII intensity map — enough to answer "is there a part in frame"
    /// without a graphics protocol the target terminals do not have; anything else gets its typed header
    /// and a hex head, which is the honest amount a terminal can say about raw bytes.
    /// </summary>
    private static string BuildPayloadBody(WatchSession.ObservedNode? node)
    {
        if (node?.LastFrame is not { } frame)
        {
            return "[grey42]this node has published no frame-data " +
                   "(run the pipeline with --egress-streams state,frame to see any)[/]";
        }

        var header =
            $"[grey62]media[/] [white]{frame.MediaType?.ToString() ?? "?"}[/]   " +
            $"[grey62]element[/] [white]{frame.ElementType?.ToString() ?? "?"}[/]   " +
            $"[grey62]shape[/] [white]{Markup.Escape(frame.ShapeText)}[/]   " +
            $"[grey62]bytes[/] [white]{FormatBytes(frame.Bytes)}[/]   " +
            $"[grey62]port[/] [white]{Markup.Escape(frame.Port)}[/]";

        if (node.LastPayload is not { Length: > 0 } payload)
        {
            return header + "\n\n[grey42]waiting for the next frame on this node...[/]";
        }

        var preview = frame.MediaType switch
        {
            PayloadMediaType.Json => PreviewJson(payload),
            _ when IsGrayscale2D(frame) => PreviewIntensity(payload, frame),
            _ => PreviewHex(payload),
        };

        return header + "\n\n" + preview;
    }

    private static string PreviewJson(byte[] payload)
    {
        var text = System.Text.Encoding.UTF8.GetString(payload, 0, Math.Min(payload.Length, 4000));
        return $"[grey78]{Markup.Escape(text)}[/]" +
               (payload.Length > 4000 ? $"\n[grey42]{Glyphs.Ellipsis} truncated[/]" : string.Empty);
    }

    /// <summary>A single-channel 8-bit image: rank 2, or rank 3 with one channel.</summary>
    private static bool IsGrayscale2D(WatchSession.FrameInfo frame) =>
        frame.ElementType == PayloadElementType.UInt8
        && frame.Shape is { } shape
        && (shape.Length == 2 || (shape.Length == 3 && shape[2] == 1))
        && shape[0] > 1 && shape[1] > 1;

    /// <summary>Coarse ASCII intensity map. ASCII on purpose — the same reason the whole glyph vocabulary
    /// is (see <see cref="Glyphs"/>): it renders identically on a panel PC's console.</summary>
    private static string PreviewIntensity(byte[] payload, WatchSession.FrameInfo frame)
    {
        const string Ramp = " .:-=+*#%@";
        const int Cols = 60;
        const int Rows = 20;

        var height = (int)frame.Shape![0];
        var width = (int)frame.Shape![1];
        if (height <= 0 || width <= 0)
        {
            return PreviewHex(payload);
        }

        var sb = new System.Text.StringBuilder();
        for (var r = 0; r < Rows; r++)
        {
            var y = (int)((r + 0.5) * height / Rows);
            for (var c = 0; c < Cols; c++)
            {
                var x = (int)((c + 0.5) * width / Cols);
                var index = (y * width) + x;
                var value = index >= 0 && index < payload.Length ? payload[index] : (byte)0;
                sb.Append(Ramp[value * (Ramp.Length - 1) / 255]);
            }

            sb.Append('\n');
        }

        return $"[grey78]{Markup.Escape(sb.ToString().TrimEnd())}[/]";
    }

    private static string PreviewHex(byte[] payload)
    {
        var take = Math.Min(payload.Length, 128);
        var sb = new System.Text.StringBuilder();
        for (var i = 0; i < take; i += 16)
        {
            for (var j = i; j < Math.Min(i + 16, take); j++)
            {
                sb.Append(payload[j].ToString("x2")).Append(' ');
            }

            sb.Append('\n');
        }

        var more = payload.Length > take ? $"[grey42]{Glyphs.Ellipsis} {FormatBytes(payload.Length - take)} more[/]" : string.Empty;
        return $"[grey78]{sb.ToString().TrimEnd()}[/]\n{more}";
    }

    private static string BuildFrameLine(WatchSession.FrameInfo frame) =>
        $"[grey62]frame[/] [white]{Markup.Escape(frame.NodeId)}[/]" +
        $"[grey42].{Markup.Escape(frame.Port)}[/] " +
        $"[grey42]{Glyphs.Separator}[/] [grey78]{frame.MediaType?.ToString() ?? "?"}[/] " +
        $"[grey62]{frame.ElementType?.ToString() ?? "?"}[/] " +
        $"[grey78]{frame.ShapeText}[/] [grey62]{FormatBytes(frame.Bytes)}[/]";

    private static string FormatMicros(long micros) => micros switch
    {
        <= 0 => Glyphs.None,
        < 1000 => $"{micros}us",
        < 1_000_000 => $"{micros / 1000.0:0.0}ms",
        _ => $"{micros / 1_000_000.0:0.00}s",
    };

    private static int SafeWidth()
    {
        try
        {
            return Console.WindowWidth > 0 ? Console.WindowWidth : 120;
        }
        catch
        {
            return 120;
        }
    }
}
