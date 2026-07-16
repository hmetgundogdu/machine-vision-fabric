# AGENTS.md

Bu dosya, `DynoVisionPipeline` reposunda çalışan AI agent'lar ve geliştiriciler için çalışma rehberidir.

## 1. Proje Amacı

DynoVisionPipeline, şirket içi ağlarda panel PC, endüstriyel PC ve NUC cihazlarda çalışan açık kaynak bir `vision integration and execution platform` hedefler.

Temel hedefler:

- farklı kamera ve image stream kaynaklarını ortak adapter modeliyle bağlamak
- strict typed node graph ile pipeline kurmak
- PLC ve benzeri kontrol entegrasyonlarını graph içinde birinci sınıf node yapmak
- AI model, Python ve external process adımlarını aynı paket içinde çalıştırmak
- edge cihazda bağımsız çalışmak, merkezi sisteme yalnızca opsiyonel telemetry ve yönetim bilgisi sunmak

## 2. Ana Mimari Kararlar

- `Windows-first` geliştirme yapılır
- runtime `headless edge engine` olarak tasarlanır
- UI `web-first Studio` yaklaşımıyla ilerler
- pipeline bağlantıları `NODE(typed output) -> NODE(typed input)` şeklinde strict ve tek yönlüdür
- `data edge` ve `control edge` farklı kavramlardır, karıştırılmaz
- merkez çalıştırma otoritesi değildir; gözlemleme ve yönetim katmanıdır

## 3. Repo Düzeni

Şu an repo erken aşamadadır. Temel alanlar:

```text
DynoVisionPipeline/
├─ docs/
├─ src/
├─ samples/
└─ tools/
```

Doküman başlangıç noktaları:

- [README.md](C:\Users\c9018243a\Desktop\Projects\DynoVisionPipeline\README.md)
- [architecture-foundation.md](C:\Users\c9018243a\Desktop\Projects\DynoVisionPipeline\docs\architecture-foundation.md)
- [technology-recommendation.md](C:\Users\c9018243a\Desktop\Projects\DynoVisionPipeline\docs\technology-recommendation.md)
- [open-questions.md](C:\Users\c9018243a\Desktop\Projects\DynoVisionPipeline\docs\open-questions.md)

Kod iskeleti oluştuğunda bu dosyayı solution ve proje bazında genişlet.

## 4. Kodlama Kuralları

- Çekirdek pipeline gevşek `dictionary` veya serbest `object` zinciri olmamalı
- Node input ve output tipleri açık şema ile tanımlanmalı
- Config şeması ile runtime payload birbirine karıştırılmamalı
- Vendor SDK entegrasyonları çekirdek koda gömülmemeli; adapter sınırında tutulmalı
- Kamera, PLC ve AI model node'larında cold start dikkate alınmalı
- Varsayılan lifecycle:
  - `source/camera`: resident
  - `plc/control`: resident
  - `ai-model`: resident + preload
  - `short helper process`: on-demand

## 5. Pipeline Tasarım Kuralları

- `data edge` veri taşır: frame, tensor, result, metadata
- `control edge` akış kararı taşır: branch, gate, trigger sonucu
- Aynı node üzerinde hem data hem control port olabilir
- Uyumlu şema yoksa node'lar bağlanamaz
- Runtime tarafı bağlantıları tekrar doğrulamalı; sadece UI doğrulamasına güvenilmez

Örnek node sınıfları:

- `source node`
- `control node`
- `compute node`
- `integration node`
- `output node`

## 6. Entegrasyon Kuralları

- Gerçek vendor adaptörleri örnek/reference paketler olarak yazılmalı
- İlk fazda gerçek kamera zorunlu değildir; güçlü simulator node'ları önceliklidir
- PLC entegrasyonu graph içinde `control node` olarak ele alınmalı
- External process ve Python çalıştırma tarafı cold start maliyeti düşünülerek resident veya on-demand seçilebilir

Simulator öncelikleri:

- `single-image-loop`
- `folder-sequence-camera`
- `side-by-side-multi-frame simulator`
- `scenario-based simulator`

## 7. Telemetry ve Merkez

- Edge cihaz merkez olmadan çalışabilmelidir
- Telemetry yayını opsiyonel olmalıdır
- Telemetry hiçbir zaman hot path'i bloklamamalıdır
- İlk tercih `WebSocket` telemetry kanalıdır
- Video ve telemetry aynı kanal üzerinden taşınmamalıdır

Merkez tarafı için uygun alanlar:

- inventory
- logs
- health
- audit
- opsiyonel pipeline event gözlemi

## 8. Paketleme Kuralları

Pipeline import/export sadece tek JSON dosyası olarak düşünülmemeli.

Beklenen yapı:

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

Bu yapı:

- model dosyalarını
- yardımcı executable'ları
- Python script'lerini
- adapter varlıklarını

aynı paket içinde taşımaya izin verir.

## 9. Agent Çalışma Pratiği

Bu repoda çalışan agent:

- önce `docs/` altındaki güncel kararları okumalı
- mimari kararlarla çelişecek geçici kısayollar eklememeli
- strict typed graph yaklaşımını zayıflatacak dinamik shortcut'lar önermemeli
- gerçek donanım şartmış gibi varsayım yapmamalı; simulator-first düşünmeli
- değişiklik yaptığında ilgili dokümanı da güncellemelidir

## 10. Henüz Netleşmemiş Alanlar

Şu konular hâlâ açık olabilir:

- Python node ilk sürüm davranış detayları
- telemetry için ileri seviye event bus ihtiyacı
- future Linux adapter kapsamı
- pipeline paketlerinde güven/imza doğrulaması

Bu alanlarda karar verirken mevcut mimari belgelerle uyum korunmalı.
