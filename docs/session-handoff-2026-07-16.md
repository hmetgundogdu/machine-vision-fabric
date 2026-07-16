# MachineVisionFabric Session Handoff

Tarih: `2026-07-16`

Bu dosya, başka bir oturumda projeye hızlı devam edebilmek için hazırlanmış konsolide durum özetidir.

## 1. Proje Özeti

Proje adı:

- `MachineVisionFabric`

Proje tipi:

- açık kaynak
- edge-first vision pipeline platformu

Amaç:

- farklı kamera ve image stream kaynaklarını ortak entegrasyon modeliyle bağlamak
- strict typed graph ile pipeline kurmak
- PLC, AI model, Python, external process ve output bileşenlerini aynı graph içinde çalıştırmak
- edge cihazda bağımsız çalışmak
- merkezden opsiyonel gözlemleme yapılabilmesini sağlamak

## 2. Operasyonel Varsayımlar

- sistem şirket ağı içinde çalışacak
- hedef cihazlar panel PC, endüstriyel PC ve Intel NUC sınıfı edge makineler
- ilk faz `Windows-first`
- internet bağlantısı zorunlu olmayacak
- merkez kapalı olsa bile edge node çalışmaya devam edecek

## 3. Ürün Yönü

Bu ürün bir tekil kamera masaüstü uygulaması olarak değil, şu yapı olarak düşünülüyor:

- graph tabanlı pipeline engine
- local edge runtime
- web tabanlı Studio
- opsiyonel merkezi gözlemleme

## 4. Netleşen Mimari Kararlar

- `cloud-first` değil, `edge-first`
- merkez `execution authority` değil, `visibility/management authority`
- her cihaz kendi lokal panelini açabilir
- merkez isterse edge cihazları canlı gözlemleyebilir
- telemetry yayını opsiyonel olacak
- telemetry hiçbir zaman hot path'i bloklamayacak

## 5. Pipeline Tasarım Kararları

### 5.1 Strict Typed Graph

Pipeline gevşek `object` veya `dictionary` zinciri olmayacak.

Temel model:

```text
NODE(typed output) -> NODE(typed input) -> NODE(typed output)
```

Kurallar:

- tüm input port'lar typed olacak
- tüm output port'lar typed olacak
- config şeması veri payload'ından ayrı olacak
- bağlantılar tek yönlü olacak
- şema uyumsuzsa node'lar bağlanamayacak

### 5.2 Data Edge ve Control Edge Ayrımı

Graph içinde iki ayrı edge tipi olacak:

- `data edge`
- `control edge`

`data edge` örnekleri:

- frame
- tensor
- inference result
- metadata

`control edge` örnekleri:

- branch kararı
- gate aç/kapat
- trigger sonucu
- PLC presence sonucu

Karar:

- `control edge` görsel olarak ayrı olacak

## 6. PLC Yaklaşımı

PLC, pipeline dışında sabit yardımcı modül gibi değil, graph içinde bir `control node` olarak ele alınacak.

Örnek kullanım:

```text
PLC Presence Node -> if product present -> Trigger/Capture -> AI Model
PLC Presence Node -> if no product -> idle branch
```

İlk PLC örneği:

- `S7-200` tabanlı control node

Amaç:

- vendor bağımlı entegrasyonları çekirdekten ayırmak
- buna rağmen graph içinde birinci sınıf node yapmak

## 7. Kamera ve Simulator Yaklaşımı

Gerçek kamera ilk MVP için zorunlu değil.

İlk fazda öncelik:

- güçlü simulator kaynakları

İlk önerilen simulator tipleri:

- `single-image-loop`
- `folder-sequence-camera`
- `side-by-side-multi-frame simulator`
- `scenario-based simulator`

Özellikle `folder-sequence-camera` için istenen davranış:

- klasördeki görüntüleri sırayla oynatmak
- döngüsel tekrar yapabilmek
- frame interval ayarı
- birden fazla sanal kamerayı aynı anda çalıştırabilmek

## 8. Lifecycle ve Cold Start Kararları

Cold start süreleri doğrudan önemli kabul edildi.

Bu yüzden node/process yaşam döngüsü pipeline kontratının parçası olacak.

Desteklenecek temel modlar:

- `resident`
- `on-demand`

Varsayılan önerilen politika:

- `camera/source`: resident
- `plc/control`: resident
- `ai-model`: resident + preload
- `short helper process`: on-demand
- `heavy external process`: varsayılan resident, override edilebilir

Bu kararın nedeni:

- kamera, PLC ve model tarafında cold start çevrim süresini bozar
- kısa işler için sürekli proses ayakta tutmak kaynak israfı olabilir

## 9. Paketleme Kararı

Pipeline import/export yalnızca tek JSON dosyası olmayacak.

Tercih edilen yapı:

- `JSON + folder package`

Beklenen paket örneği:

```text
pipeline-package/
├─ pipeline.json
├─ assets/
│  ├─ models/
│  ├─ scripts/
│  ├─ processes/
│  └─ configs/
└─ manifest.json
```

Sebep:

- ONNX model dosyaları
- Python script'leri
- yardımcı executable'lar
- config dosyaları
- adapter manifestleri

aynı paket içinde taşınabilsin.

## 10. Merkezi Sistem Yaklaşımı

Tam merkezi orchestrator istenmiyor.

Tercih edilen hibrit model:

### Edge tarafında kalacaklar

- kamera bağlantısı
- frame alma
- trigger engine
- graph execution
- AI inference
- yerel stream üretimi
- kısa süreli persist

### Merkezde olabilecekler

- inventory
- logs
- health
- audit
- opsiyonel pipeline event gözlemi
- opsiyonel pipeline şablon arşivi

Merkez için önemli sınır:

- çalıştırma otoritesi olmayacak

## 11. Telemetry Kararı

Merkez isterse edge pipeline sinyallerini izleyebilecek.

Kurallar:

- telemetry opsiyonel
- varsayılan olarak kapalı olabilir
- best-effort çalışacak
- edge hot path'i bloklamayacak
- buffer dolarsa telemetry düşebilir, pipeline durmamalı

İlk tercih edilen protokol:

- `WebSocket`

Neden:

- lokal web panel ile doğal uyum
- LAN içinde yeterli performans
- raw TCP'ye göre daha düşük uygulama maliyeti

İleride gerekirse:

- event bus
- NATS benzeri yaklaşım

## 12. Teknoloji Kararları

Önerilen teknoloji seti:

- `Engine`: `C# / .NET 10 LTS`
- `Studio`: `React + TypeScript + React Flow`
- `Desktop shell`: gerekirse `Tauri`
- `Inference`: `ONNX Runtime`
- `Streaming`: önce `MediaMTX`, gerekirse sonra `GStreamer`
- `Python`: ayrı worker/external process
- `Vendor SDK`: adapter paketi + native interop

Ek not:

- UI içine kamera SDK veya runtime gömülmeyecek
- headless runtime ile UI ayrı tutulacak

## 13. Açık Kaynak ve Entegrasyon Yaklaşımı

- vendor kamera entegrasyonları çekirdeğe gömülü olmayacak
- bunlar örnek/reference adapter paketleri olarak yazılacak
- aynı yaklaşım PLC entegrasyonları için de geçerli olacak
- çekirdek mümkün olduğunca vendor bağımsız kalacak

## 14. Bu Oturumda Oluşturulan Ana Dokümanlar

- [README.md](C:\Users\c9018243a\Desktop\Projects\machine-vision-fabric\README.md)
- [architecture-foundation.md](C:\Users\c9018243a\Desktop\Projects\machine-vision-fabric\docs\architecture-foundation.md)
- [legacy-project-findings.md](C:\Users\c9018243a\Desktop\Projects\machine-vision-fabric\docs\legacy-project-findings.md)
- [technology-recommendation.md](C:\Users\c9018243a\Desktop\Projects\machine-vision-fabric\docs\technology-recommendation.md)
- [open-questions.md](C:\Users\c9018243a\Desktop\Projects\machine-vision-fabric\docs\open-questions.md)
- [AGENTS.md](C:\Users\c9018243a\Desktop\Projects\machine-vision-fabric\AGENTS.md)
- [CLAUDE.md](C:\Users\c9018243a\Desktop\Projects\machine-vision-fabric\CLAUDE.md)

## 15. Kalan Açık Sorular

Henüz tam kapanmamış ama bloklayıcı olmayan konular:

1. Diagram editörü yalnızca config mi üretecek, yoksa canlı runtime müdahalesi de sağlayacak mı?
2. Python node ilk sürümde yalnızca `external executable` mi olacak, yoksa `resident sidecar worker` da gelecek mi?
3. İlk ağ yayın önceliği RTSP/SRT mi, yoksa önce metadata/event akışı mı?
4. Telemetry buffer dolunca hangi event'ler düşürülebilir?
5. İleride paketlerde imza/güven doğrulaması gerekecek mi?

## 16. Önerilen Sonraki Adım

En mantıklı bir sonraki çalışma:

1. solution yapısını tanımlamak
2. proje isimlerini netleştirmek
3. node contract şemasını yazmak
4. pipeline package şemasını yazmak
5. ilk MVP kapsamını sabitlemek

Önerilen ilk MVP kapsamı:

- `runtime`
- `local studio`
- `PLC control node`
- `multi-simulator source`
- `ONNX node`
- `package import/export`

## 17. Hızlı Devam Komutu

Başka bir oturumda devam ederken ilk okunacak dosya bu olsun:

- [session-handoff-2026-07-16.md](C:\Users\c9018243a\Desktop\Projects\machine-vision-fabric\docs\session-handoff-2026-07-16.md)

Sonra sırasıyla:

- [architecture-foundation.md](C:\Users\c9018243a\Desktop\Projects\machine-vision-fabric\docs\architecture-foundation.md)
- [technology-recommendation.md](C:\Users\c9018243a\Desktop\Projects\machine-vision-fabric\docs\technology-recommendation.md)
