using System.Net;
using System.Net.Sockets;
using System.Text;

var options = SimulatorInvocation.Parse(args);
if (options.ShowHelp)
{
    PrintHelp();
    return;
}

var listener = new TcpListener(IPAddress.Parse(options.Host), options.Port);
listener.Start();

Console.WriteLine($"Listening on {options.Host}:{options.Port}");
Console.WriteLine($"Signal value: {options.ResponseValue}");
Console.WriteLine("Press Ctrl+C to stop.");

while (true)
{
    using var client = await listener.AcceptTcpClientAsync();
    await using var stream = client.GetStream();
    using var reader = new StreamReader(stream, Encoding.UTF8, leaveOpen: true);
    await reader.ReadLineAsync();

    var responseBytes = Encoding.UTF8.GetBytes(options.ResponseValue + "\n");
    await stream.WriteAsync(responseBytes);
    await stream.FlushAsync();
}

static void PrintHelp()
{
    Console.WriteLine("MachineVisionFabric.TcpSignalSimulator");
    Console.WriteLine("Options:");
    Console.WriteLine("  --host <ip>       Default: 127.0.0.1");
    Console.WriteLine("  --port <number>   Default: 15020");
    Console.WriteLine("  --value <text>    Default: 1");
}

internal sealed class SimulatorInvocation
{
    public required bool ShowHelp { get; init; }
    public required string Host { get; init; }
    public required int Port { get; init; }
    public required string ResponseValue { get; init; }

    public static SimulatorInvocation Parse(string[] args)
    {
        if (args.Any(arg => arg is "--help" or "-h" or "help"))
        {
            return new SimulatorInvocation
            {
                ShowHelp = true,
                Host = "127.0.0.1",
                Port = 15020,
                ResponseValue = "1"
            };
        }

        var options = new Dictionary<string, string>(StringComparer.OrdinalIgnoreCase);
        for (var i = 0; i < args.Length; i++)
        {
            var token = args[i];
            if (!token.StartsWith("--", StringComparison.Ordinal))
            {
                continue;
            }

            var key = token[2..];
            var value = i + 1 < args.Length && !args[i + 1].StartsWith("--", StringComparison.Ordinal)
                ? args[++i]
                : "true";
            options[key] = value;
        }

        return new SimulatorInvocation
        {
            ShowHelp = false,
            Host = options.TryGetValue("host", out var host) ? host : "127.0.0.1",
            Port = options.TryGetValue("port", out var port) && int.TryParse(port, out var parsedPort) ? parsedPort : 15020,
            ResponseValue = options.TryGetValue("value", out var responseValue) ? responseValue : "1"
        };
    }
}
