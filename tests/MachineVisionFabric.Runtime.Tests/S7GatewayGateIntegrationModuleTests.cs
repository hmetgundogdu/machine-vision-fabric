using System.Net;
using System.Net.Sockets;
using System.Text;
using System.Text.Json;
using MachineVisionFabric.Contracts.Control;

namespace MachineVisionFabric.Runtime.Tests;

public sealed class S7GatewayGateIntegrationModuleTests
{
    [Fact]
    public async Task EvaluateAsync_SendsS7StyleReadRequestAndReturnsProductPresent()
    {
        using var listener = new TcpListener(IPAddress.Loopback, 0);
        listener.Start();
        var port = ((IPEndPoint)listener.LocalEndpoint).Port;
        var capturedRequest = string.Empty;

        var serverTask = Task.Run(async () =>
        {
            using var client = await listener.AcceptTcpClientAsync();
            await using var stream = client.GetStream();
            using var reader = new StreamReader(stream, Encoding.UTF8, leaveOpen: true);
            capturedRequest = (await reader.ReadLineAsync()) ?? string.Empty;
            var bytes = Encoding.UTF8.GetBytes("1\n");
            await stream.WriteAsync(bytes);
            await stream.FlushAsync();
        });

        var module = new MachineVisionFabric.Integrations.S7GatewayGate.S7GatewayGateIntegrationModule();
        var gate = module.CreateGate(JsonSerializer.SerializeToElement(new S7GatewayGateOptions
        {
            Host = "127.0.0.1",
            Port = port,
            Rack = 0,
            Slot = 1,
            Address = new S7SignalAddress
            {
                DataBlockNumber = 5,
                ByteOffset = 2,
                BitOffset = 1
            },
            SourceName = "s7-gateway-test",
            StationId = "station-s7"
        }));

        var decision = await gate.EvaluateAsync(CancellationToken.None);
        await serverTask;

        Assert.True(decision.ProductPresent);
        Assert.Equal("s7-gateway-test", decision.Source);
        Assert.Equal("station-s7", decision.StationId);
        Assert.Equal("READ rack=0;slot=1;db=5;byte=2;bit=1", capturedRequest);
    }
}
