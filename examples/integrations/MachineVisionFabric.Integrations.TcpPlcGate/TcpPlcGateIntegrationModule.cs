using System.Net.Sockets;
using System.Text;
using System.Text.Json;
using MachineVisionFabric.Contracts.Control;
using MachineVisionFabric.Contracts.Integrations;
using MachineVisionFabric.Core.Abstractions;
using MachineVisionFabric.Sdk;

namespace MachineVisionFabric.Integrations.TcpPlcGate;

public sealed class TcpPlcGateIntegrationModule : IProductPresenceGateModule
{
    private static readonly JsonSerializerOptions JsonOptions = new(JsonSerializerDefaults.Web);

    public IntegrationModuleDescriptor Describe()
    {
        return IntegrationModuleDescriptorBuilder.CreateGate<TcpSignalGateOptions>(
            "mvf.tcp-plc-gate",
            "TCP PLC Product Presence Gate",
            "0.1.0",
            "tcp-product-presence-gate",
            "Reads a product presence signal from a simple TCP line-based endpoint.");
    }

    public IProductPresenceGate CreateGate(JsonElement configuration)
    {
        var options = configuration.ValueKind is JsonValueKind.Undefined or JsonValueKind.Null
            ? new TcpSignalGateOptions()
            : JsonSerializer.Deserialize<TcpSignalGateOptions>(configuration.GetRawText(), JsonOptions) ?? new TcpSignalGateOptions();

        return new TcpPlcGate(options);
    }

    private sealed class TcpPlcGate(TcpSignalGateOptions options) : IProductPresenceGate
    {
        public async Task<ProductPresenceDecision> EvaluateAsync(CancellationToken cancellationToken)
        {
            using var client = new TcpClient();

            using var connectCts = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
            connectCts.CancelAfter(options.ConnectTimeoutMs);
            await client.ConnectAsync(options.Host, options.Port, connectCts.Token);

            await using var stream = client.GetStream();
            stream.ReadTimeout = options.ReadTimeoutMs;
            stream.WriteTimeout = options.ReadTimeoutMs;

            var requestBytes = Encoding.UTF8.GetBytes(options.RequestPayload + "\n");
            await stream.WriteAsync(requestBytes, cancellationToken);
            await stream.FlushAsync(cancellationToken);

            using var readCts = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
            readCts.CancelAfter(options.ReadTimeoutMs);
            using var reader = new StreamReader(stream, Encoding.UTF8, leaveOpen: true);
            var response = (await reader.ReadLineAsync(readCts.Token))?.Trim();

            if (string.IsNullOrWhiteSpace(response))
            {
                throw new InvalidOperationException("TCP PLC gate returned an empty response.");
            }

            var productPresent = ResolveProductPresent(response, options);
            return new ProductPresenceDecision(
                productPresent,
                options.SourceName,
                options.StationId,
                DateTime.UtcNow,
                $"tcp-response={response}");
        }

        private static bool ResolveProductPresent(string response, TcpSignalGateOptions options)
        {
            if (string.Equals(response, options.ProductPresentValue, StringComparison.OrdinalIgnoreCase))
            {
                return true;
            }

            if (string.Equals(response, options.ProductAbsentValue, StringComparison.OrdinalIgnoreCase))
            {
                return false;
            }

            if (bool.TryParse(response, out var parsedBool))
            {
                return parsedBool;
            }

            throw new InvalidOperationException(
                $"TCP PLC gate returned '{response}', which does not match present='{options.ProductPresentValue}' or absent='{options.ProductAbsentValue}'.");
        }
    }
}
