# Açık Sorular

Bu sorular sonraki adımı daraltmak için hâlâ değerli:

1. Diagram editörü sadece konfigürasyon mu üretecek, yoksa canlı runtime gözlemi ve sınırlı müdahale de yapacak mı?
2. Python node'ları ilk sürümde sadece `external executable` olarak mı başlayacak, yoksa `resident sidecar worker` da ilk fazda gelecek mi?
3. Ağ yayını için ilk gerçek öncelik hangisi: cihazlar arası RTSP/SRT mi, yoksa önce metadata/event akışı mı?
4. Telemetry buffer dolduğunda hangi olaylar düşürülebilir, hangileri mutlaka yerel log'a da yazılmalı?
5. Pipeline klasör paketinde dış process dosyaları için ileride güven/imza doğrulaması gerekecek mi?

Şu kararlar artık net kabul ediliyor:

- `Windows-first`
- strict typed graph
- ayrı `data edge` ve `control edge`
- `PLC control node` graph içinde birinci sınıf node
- `JSON + folder package` import/export
- telemetry opsiyonel ve non-blocking
- ilk telemetry tercihi `WebSocket`
- gerçek kamera ilk MVP için zorunlu değil
- multi-simulator source yaklaşımı tercih ediliyor

Netleşmesi gereken en yakın ürün kararı:

- `MVP önce runtime + local studio + PLC control node + multi-simulator source` mu olacak
- yoksa ilk sprintte temel ONNX node ve package import/export da birlikte mi istenecek
