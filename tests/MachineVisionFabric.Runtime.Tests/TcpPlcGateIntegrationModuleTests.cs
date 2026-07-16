using System.Net;
using System.Net.Sockets;
using System.Text;
using System.Text.Json;
using MachineVisionFabric.Contracts.Control;

namespace MachineVisionFabric.Runtime.Tests;

public sealed class TcpPlcGateIntegrationModuleTests
{
    [Fact]
    public async Task EvaluateAsync_ReturnsProductPresent_WhenTcpSignalMatchesConfiguredPresentValue()
    {
        using var listener = new TcpListener(IPAddress.Loopback, 0);
        listener.Start();
        var port = ((IPEndPoint)listener.LocalEndpoint).Port;

        var serverTask = Task.Run(async () =>
        {
            using var client = await listener.AcceptTcpClientAsync();
            await using var stream = client.GetStream();
            using var reader = new StreamReader(stream, Encoding.UTF8, leaveOpen: true);
            await reader.ReadLineAsync();
            var bytes = Encoding.UTF8.GetBytes("1\n");
            await stream.WriteAsync(bytes);
            await stream.FlushAsync();
        });

        var module = new MachineVisionFabric.Integrations.TcpPlcGate.TcpPlcGateIntegrationModule();
        var gate = module.CreateGate(JsonSerializer.SerializeToElement(new TcpSignalGateOptions
        {
            Host = "127.0.0.1",
            Port = port,
            ProductPresentValue = "1",
            ProductAbsentValue = "0",
            SourceName = "tcp-test-gate",
            StationId = "station-tcp"
        }));

        var decision = await gate.EvaluateAsync(CancellationToken.None);
        await serverTask;

        Assert.True(decision.ProductPresent);
        Assert.Equal("tcp-test-gate", decision.Source);
        Assert.Equal("station-tcp", decision.StationId);
    }

    [Fact]
    public async Task EvaluateAsync_ReturnsProductAbsent_WhenTcpSignalMatchesConfiguredAbsentValue()
    {
        using var listener = new TcpListener(IPAddress.Loopback, 0);
        listener.Start();
        var port = ((IPEndPoint)listener.LocalEndpoint).Port;

        var serverTask = Task.Run(async () =>
        {
            using var client = await listener.AcceptTcpClientAsync();
            await using var stream = client.GetStream();
            using var reader = new StreamReader(stream, Encoding.UTF8, leaveOpen: true);
            await reader.ReadLineAsync();
            var bytes = Encoding.UTF8.GetBytes("0\n");
            await stream.WriteAsync(bytes);
            await stream.FlushAsync();
        });

        var module = new MachineVisionFabric.Integrations.TcpPlcGate.TcpPlcGateIntegrationModule();
        var gate = module.CreateGate(JsonSerializer.SerializeToElement(new TcpSignalGateOptions
        {
            Host = "127.0.0.1",
            Port = port,
            ProductPresentValue = "1",
            ProductAbsentValue = "0"
        }));

        var decision = await gate.EvaluateAsync(CancellationToken.None);
        await serverTask;

        Assert.False(decision.ProductPresent);
    }
}
