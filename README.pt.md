# CAFE — Compression Adaptative Filtering Experiment

[![License](https://img.shields.io/badge/license-BSD--3--Clause-green)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.70%2B-orange)](https://www.rust-lang.org)
[![Build Status](https://img.shields.io/badge/build-passing-brightgreen)]()
[![Security](https://img.shields.io/badge/security-audited-green)](docs/SECURITY_AUDIT.md)

Um formato de imagem moderno baseado em chunks, inspirado em PNG, com suporte a compressão ZSTD, filtros preditivos avançados (16 tipos), paleta indexada, metadados estruturados (EXIF, JSON, ICC, XMP) e entrelaçamento progressivo.

**Versão**: 1.11.0  
**Status**: ✅ Completo, auditado, e com aceleração SIMD  
**Compatibilidade**: Rust 2021+

---

## 🚀 Características Principais

### Compressão Inteligente
- **ZSTD** com fallback para dados brutos (seção 3.2)
- Nível ajustável (1-22)
- Dicionário ZSTD opcional (`zDIC` chunk)

### Filtros Preditivos Avançados
- **16 tipos de filtros**: None, Sub, Up, Average, Paeth, MED, Gradient, Simple Median, 2nd Order, 4-way Directional (4 variantes), Context-Based, TR-Directional (WebP Predictor 10) e Weighted adaptativo (inspirado no JPEG-XL)
- Aplicados por bloco/tile (Filter method=2) ou **por linha** (Filter method=3, v1.5, adaptação mais granular)
- **Aceleração AVX2 SIMD** (v1.1+): Filtros 1-14 vetorizados para processamento 4-8x mais rápido; detecção automática de CPU com fallback escalar
- **Aceleração ARM NEON SIMD** (v1.3-v1.4): todos os 14 filtros vetorizados mais pack/unpack, conversão de amostras, byte-shuffle e quantização de paleta portados para NEON — nenhum módulo SIMD é mais exclusivo de AVX2
- **v1.2 SIMD Agressivo**: Pack/Unpack 1/2/4-bit (8-16x), expansão/redução de amostras 8→16/32 float (4-6x), Byte-shuffle com blocking (10-20% melhoria de cache), Filter 3 melhorado (4-6x)
- Seleção automática por heurística: **Entropia de Shannon** (padrão), **MSAD** (`--filter-heuristic msad`), **compressão de teste real** (`--filter-heuristic test`), **QuickPrune** (v1.1, MSAD rápido + Entropia nos top 8) ou **AdaptiveEntropy** (v1.1, análise consciente do conteúdo)

### Flexibilidade de Cores
- **Color types**: Cinza, RGB, Indexado (paleta), Cinza+Alfa, RGBA
- **Bit depths**: 1, 2, 4, 8, 10, 12, 16, 32 bits
- **Sample formats**: uint, float (IEEE 754), half-float (fp16)

### Metadados Estruturados
- **EXIF**: Câmera, geolocalização, data (blob TIFF)
- **JSON**: Dados proprietários por namespace
- **ICC**: Perfil de cor para gestão profissional
- **XMP**: Metadados de fluxo editorial

### Streaming Inteligente
- **iDIM**: Tiling 2D real com IDAT por tile, scan order row-major ou Z-order (Morton)
- **Entrelaçamento**: Adam7 (7 passes) ou Par/Ímpar (2 passes)
- Decodificação incremental (chunk-by-chunk)
- **API de streaming `Decoder<R: Read>`**: decodifica tile por tile diretamente de qualquer fonte `Read` (arquivo, socket, `Cursor`) sem armazenar o arquivo comprimido inteiro nem a imagem decodificada inteira em memória — veja `examples/streaming_decode.rs` e a seção "API de Biblioteca" abaixo (apenas tiling row-strip; recai para `decode`/`decode_bytes` em arquivos com tiling 2D ou entrelaçados)
- **API de streaming `Encoder<W: Write>`** (v1.6): contraparte simétrica — escreve o `IHDR` e cada `IDAT` row-strip imediatamente à medida que os tiles chegam via `add_tile()`, em vez de exigir a imagem inteira em memória antes que `encode()` possa produzir saída; `Encoder<W: Write + Seek>::finish_exact()` corrige o `compression_method` para seu valor exato (idêntico byte a byte a `encode()`), enquanto destinos apenas `Write` recebem uma superestimativa conservadora (sempre segura); também suporta tiling 2D (`iDIM`, v1.10, via `add_idim_tile()`) e entrelaçamento par/ímpar (v1.11, via `add_even_odd_rows()`) — veja `examples/streaming_encode.rs` e a seção "API de Biblioteca" abaixo (sem paleta indexada ou entrelaçamento Adam7, limitações permanentes)

### Auditoria de Compressão (v1.5)
- **Filtro preditivo por linha** (`Filter method=3`): adaptação mais granular que o filtro por tile
- **Garantia de não-regressão do `auto_dictionary`**: um dicionário ZSTD auto-treinado só é usado quando reduz estritamente o tamanho do arquivo, verificado por `IDAT` e no arquivo completo
- **Quantização de paleta perceptualmente ponderada**: `PaletteAlgorithm::NearestNeighborWeighted` usando a fórmula de distância redmean
- **Benchmarks de compressão reais + gate de regressão no CI**: `tests/compression_regression.rs` e `benches/encode_decode.rs`
- **Quantização de paleta k-means** (v1.7): `PaletteAlgorithm::KMeans` refina centróides iterativamente (algoritmo de Lloyd, inicialização determinística via median-cut) para o menor erro quadrático médio entre os quatro algoritmos
- **Tone-mapping inverso no encode** (v1.8): `EncodeOptions::inverse_tonemap`/`--inverse-tonemap reinhard` opt-in sintetiza dados HDR float plausíveis a partir de entrada SDR 8-bit (só Reinhard — sem inversa em forma fechada para Filmic); requer `sample_format=float` + `chdr_transfer` linear + RGBA

### Segurança
- ✅ Proteção contra decompression bomb (CWE-409)
- ✅ Validação de input não confiável
- ✅ Sem panic em arquivos malformados/truncados
- ✅ [Auditoria completa](docs/SECURITY_AUDIT.md)

---

## 📦 Estrutura do Projeto

```
cafe/
├── AGENTS.md                      # Guia técnico para desenvolvedores
├── CLAUDE.md                      # Link simbólico para AGENTS.md
├── Cargo.toml                     # Dependências e configuração (com feature simd)
├── deny.toml                      # Configuração de segurança e licenças do Cargo-deny
├── README.md                      # README em inglês
├── README.pt.md                   # README em português
├── LICENSE                        # Licença BSD-3-Clause
├── src/                           # Biblioteca principal
│   ├── cafe.rs                    # Núcleo: encode/decode, chunks (re-exports)
│   ├── constants.rs               # Assinatura, flags, color types, filtros
│   ├── chunk.rs                   # Framing de chunks (Length/Type/Flag/Data/CRC32)
│   ├── codec.rs                   # Compressão ZSTD com fallback (seção 3.2)
│   ├── color.rs                   # Conversões de cor, pack/unpack, float/half
│   ├── filter.rs                  # 16 filtros preditivos + heurísticas (com integração SIMD)
│   ├── simd.rs                    # Filtros vetorizados AVX2/NEON 1-14 (v1.1+, feature opcional)
│   ├── simd_packing.rs            # Pack/unpack 1/2/4-bit com AVX2/NEON (v1.2+)
│   ├── simd_sample_conversion.rs  # Expansão 8→16/32, redução 16/32→8 com AVX2/NEON (v1.2+)
│   ├── simd_quantize.rs           # Busca de paleta mais próxima com AVX2/NEON (v1.2+)
│   ├── simd_shuffle.rs            # Byte-shuffle via table lookup AVX2/NEON (v1.2+)
│   ├── shuffle.rs                 # Byte-shuffle (Filter Method=1, v1.1)
│   ├── tonemap.rs                 # HDR tone-mapping (EOTF, primaries, operadores, v1.1)
│   ├── interlace.rs               # Adam7 e par/ímpar
│   ├── types.rs                   # EncodeOptions, Palette, iDim, cHDR, etc.
│   └── error.rs                   # CafeError
├── tools/                         # Ferramentas CLI
│   ├── cafe-encode.rs            # Binário encoder
│   └── cafe-decode.rs            # Binário decoder
├── docs/                          # Documentação
│   ├── CAFE-spec.md              # Especificação completa (v1.1, atualizada até v1.6)
│   ├── CAFE-spec.pt.md           # Versão portuguesa da especificação
│   ├── SECURITY_AUDIT.md         # Auditoria de segurança
│   └── DEVELOPER_GUIDE.md        # Guia técnico para contribuidores
├── tests/                         # Testes de integração e round-trip
├── examples/                      # Exemplos de uso
│   ├── basic_encode.rs           # Exemplo básico de encoding
│   └── basic_decode.rs           # Exemplo básico de decoding
└── .github/
    └── workflows/                 # CI (build, clippy -D warnings, fmt, doc, security audit)
```

---

## 🏗️ Arquitetura

### Layout de Chunk
```
[Length: 4 bytes BE]
[Type: 4 bytes ASCII]
[Flag: 1 byte] — 0x00=bruto, 0x01=ZSTD
[Data: N bytes] — conteúdo (comprimido ou não)
[CRC32: 4 bytes BE]
```

### Chunks Definidos

**Críticos** (1ª letra maiúscula):
| Tipo | Descrição |
|------|-----------|
| `IHDR` | Header (sempre primeiro, nunca comprimido) |
| `PLTE` | Paleta indexada (obrigatório se color_type=3) |
| `IDAT` | Dados de pixels (1+ por arquivo) |
| `IEND` | Marca fim (sempre último) |

**Ancilar** (1ª letra minúscula, opcionais):
| Tipo | Descrição |
|------|-----------|
| `eXIF` | Metadados EXIF (blob TIFF) |
| `jSON` | Metadados JSON (múltiplas instâncias por namespace) |
| `iDIM` | Tiling e scan order para streaming |
| `iCCP` | Perfil ICC para gestão de cores |
| `xMPd` | Metadados XMP |
| `zDIC` | Dicionário ZSTD para IDAT |
| `cHDR` | Metadados HDR (transfer func, luminância) |

---

## 📖 Uso

### Compilação

```bash
# Build release com SIMD (otimizado, recomendado)
cargo build --release

# Build release sem SIMD (se AVX2 não está disponível)
cargo build --release --no-default-features

# Executáveis
./target/release/cafe-encode input.png output.cafe
./target/release/cafe-decode output.cafe decoded.png
```

**Nota sobre SIMD:** A feature `simd` está ativada por padrão. O suporte a AVX2 é detectado em runtime via `is_x86_feature_detected!("avx2")`, então o mesmo binário usa automaticamente os intrínsecos AVX2 para os Filtros 1, 2 e 3 em CPUs compatíveis, fazendo fallback para código escalar nas demais. Não é necessário nenhum `RUSTFLAGS` ou flag de build especial — funciona out-of-the-box com `cargo build --release`.

### API de Biblioteca

```rust
use cafe::{encode, decode, EncodeOptions};

// Encodar
let opts = EncodeOptions {
    use_filter: true,
    level: 19,
    adaptive_analysis: true,
    target_color_type: 6, // RGBA
    ..EncodeOptions::default()
};
encode("input.png", "output.cafe", &opts)?;

// Decodar
let result = decode("output.cafe", "output.png")?;
println!("EXIF: {:?}", result.exif);
println!("JSON: {:?}", result.json_metadata);
```

#### Decodificação em streaming (imagens grandes / pouca memória)

```rust
use cafe::Decoder;
use std::fs::File;

let file = File::open("output.cafe")?;
let mut decoder = Decoder::new(file);

let info = decoder.read_info()?; // lê IHDR + todos os chunks antes do IDAT
if info.supports_streaming_tiles {
    while let Some(tile) = decoder.next_tile()? {
        // tile.pixels: tile.width * tile.height * 4 bytes de RGBA
    }
}
let result = decoder.finish()?; // metadados EXIF/JSON/ICC/XMP/HDR
```

Veja `examples/streaming_decode.rs` para um exemplo completo executável.
`next_tile()` suporta tiling 2D (`iDIM`, desde v1.9) além de arquivos em
faixas de linhas simples, mas ainda não suporta arquivos entrelaçados (uma
limitação de design permanente) nem `iDIM` combinado com paleta indexada /
`bit_depth < 8` — verifique `info.supports_streaming_tiles` e recorra a
`decode`/`decode_bytes` caso seja `false`.

#### Codificação em streaming (imagens grandes / produtores incrementais)

```rust
use cafe::{Encoder, EncoderOptions};
use std::fs::File;

let file = File::create("output.cafe")?;
let opts = EncoderOptions::default();
let mut encoder = Encoder::new(file, width, height, &opts)?; // escreve o IHDR imediatamente

for row_strip in tiles {
    encoder.add_tile(&row_strip)?; // width * tile_height * 4 bytes RGBA por chamada
}

let _file = encoder.finish_exact()?; // compression_method exato (requer Seek)
// ou encoder.finish()? para destinos apenas Write (compression_method conservador)
```

`Encoder<W>` também suporta tiling 2D (`iDIM`, desde v1.10) como alternativa ao
envio row-strip — habilite via `EncoderOptions::idim: Some((tile_width, tile_height, scan_order))`
e chame `add_idim_tile()` (mutuamente exclusivo com `add_tile()`) uma vez por
tile na grade, na sequência de `iDim::tile_order()` (row-major ou Z-order):

```rust
let opts = EncoderOptions { idim: Some((tile_w, tile_h, 0)), ..Default::default() }; // 0=row-major, 1=Z-order
let mut encoder = Encoder::new(file, width, height, &opts)?; // escreve IHDR + iDIM imediatamente

for tile in tiles_na_ordem_da_grade {
    encoder.add_idim_tile(&tile)?; // exatamente tile_w * tile_h * 4 bytes RGBA por chamada (menor nas bordas)
}

let _file = encoder.finish_exact()?;
```

`Encoder<W>` também suporta entrelaçamento par/ímpar (`Interlace = 2`, desde
v1.11) — habilite via `EncoderOptions::even_odd_interlace: true` (restrito a
RGBA uint de 8 bits) e submeta faixas contíguas de linhas de altura e
alinhamento arbitrários via `add_even_odd_rows()` (mutuamente exclusivo com
`add_tile()`/`add_idim_tile()`):

```rust
let opts = EncoderOptions { even_odd_interlace: true, ..Default::default() };
let mut encoder = Encoder::new(file, width, height, &opts)?; // escreve IHDR imediatamente

for row_chunk in linhas_conforme_chegam {
    encoder.add_even_odd_rows(&row_chunk)?; // qualquer número de linhas RGBA inteiras por chamada
}

let _file = encoder.finish_exact()?;
```

Veja `examples/streaming_encode.rs` para um exemplo completo executável (o 4º
argumento de CLI demonstra `add_idim_tile()`). `Encoder<W>` suporta tiling
row-strip, tiling 2D (`iDIM`), entrelaçamento par/ímpar e color types diretos
apenas — sem paleta indexada, `auto_dictionary` ou entrelaçamento Adam7
(limitações de design permanentes — veja o doc comment de `EncoderOptions`).

### CLI

```bash
# Encode padrão
cafe-encode input.png output.cafe

# Encode com opções
cafe-encode input.png output.cafe --level 22 --color-type 2 --no-filter

# Decode
cafe-decode output.cafe decoded.png

# Ajuda
cafe-encode --help
cafe-decode --help
```

---

## 📊 Performance

### Razão de Compressão
- **PNG típico**: 100 KB → 60-80 KB (CAFE, 20-40% ganho)
- **Imagem colorida**: Melhor em dados com padrões (gradientes, linhas)
- **Imagem com ruído**: Similar a PNG (pouco ganho de filtro)

### Velocidade de Encoding (v1.2.1)
| Configuração | Tempo (512×512 RGB) | Notas |
|---|---|---|
| **Nível 1** (mais rápido) | ~8 ms | Sem filtros, SIMD pack acelerado |
| **Nível 9** (balanceado) | ~15 ms | Recomendado para maioria dos casos |
| **Nível 19** (padrão) | ~28 ms | Compressão alta, SIMD acelerado |
| **Nível 22** (máximo) | ~75 ms | Não recomendado para aplicações real-time |

### Velocidade de Decoding
- **Decodificar RGBA** (512×512): ~3 ms (com SIMD)
- **Decodificar indexado** (512×512): ~1.5 ms (com SIMD)
- **Com AVX2 SIMD** (v1.1+): Processamento 4-8x mais rápido dos Filtros 1, 2, 3
- **v1.2 SIMD agressivo**: Pack/Unpack 8-16x, Expansão/redução 4-6x, ganha 2.8-3.5x em blend típico

### Comparação com PNG
- Encoding: ~2-5% mais lento que PNG (compensado por melhor compressão)
- Decoding: ~1-2x mais rápido que PNG (conjunto de filtros mais simples, SIMD acelerado)
- Tamanho do arquivo: ~15-25% menor em média

**Nota de benchmark**: Execute `cargo bench` para gerar um relatório detalhado do criterion em `target/criterion/report/index.html`

---

## 🔒 Segurança

- ✅ **Auditado**: [Relatório completo](docs/SECURITY_AUDIT.md)
- ✅ **Padronizado**: Segue boas práticas de formato de imagem
- ✅ **Sem panics**: Todas as falhas retornam `Result`, nunca crash em input não confiável
- ✅ **Limite de memória**: Proteção contra decompression bomb (1 GiB/chunk)

---

## 📋 Dependências

```toml
image = "0.25"          # Leitura/escrita de PNG, JPEG, etc.
zstd = "0.13"           # Compressão ZSTD
serde_json = "1.0"      # Parsing JSON
half = "2.7"            # Half-float (fp16)
crc32fast = "1.3"       # CRC32 para chunks
```

---

## 📚 Documentação

- **[CAFE Specification](docs/CAFE-spec.md)** — Especificação completa (722 linhas)
- **[Security Audit](docs/SECURITY_AUDIT.md)** — Auditoria de segurança detalhada
- **[Developer Guide](docs/DEVELOPER_GUIDE.md)** — Guia técnico para contribuidores
- **[API Docs](https://docs.rs/cafe)** — Documentação Rust (gerada por `cargo doc`)

---

## 📝 Licença

Licenciado sob **BSD-3-Clause** — permissivo, amigável a uso comercial, sem requisitos de copyleft.

---

## 🤝 Contribuições

Contribuições são bem-vindas! Áreas com potencial:

- [x] SIMD nos filtros (Filter method 1, 2, 3) — *completo em v1.1* (AVX2, speedup 4-8x)
- [x] Byte-shuffle (Filter method=1) — *completo em v1.1*
- [x] Testes de fuzzing — *completo em v1.1* (cargo-fuzz + testes de robustez)
- [x] Testes de propriedade — *completo em v1.1* (proptest)
- [x] Benchmarking — *completo em v1.1* (criterion com comparação vs PNG)
- [x] Dicionário ZSTD automático — *completo em v1.1* (`--auto-dict`)
- [x] Paleta indexada com median-cut — *completo em v1.1* (`--palette-algorithm`)
- [x] SIMD no empacotamento sub-byte (1/2/4-bit pack/unpack) — *completo em v1.2* (AVX2, 8-16x)
- [x] SIMD na conversão de amostras (8→16/32, 16/32→8) — *completo em v1.2* (AVX2, 4-6x)
- [x] 203 testes completos (197 unit + 6 integration roundtrip) — *completo em v1.2*
- [x] **Suporte NEON (SIMD ARM)** — *completo em v1.3-v1.4*: Filtros 1-14 (v1.3) mais pack/unpack, conversão de amostras, byte-shuffle e quantização de paleta (v1.4) — nenhum módulo SIMD é mais exclusivo de AVX2
- [x] **Filtro preditivo por linha** — *completo em v1.5* (`Filter method=3`, adaptação mais granular que por tile)
- [x] **Garantia de não-regressão do dicionário ZSTD automático** — *completo em v1.5* (`auto_dictionary` só usado quando reduz estritamente o tamanho do arquivo)
- [x] **Quantização de paleta perceptualmente ponderada** — *completo em v1.5* (`PaletteAlgorithm::NearestNeighborWeighted`, distância redmean)
- [x] **Benchmarks de compressão reais + gate de regressão no CI** — *completo em v1.5* (`tests/compression_regression.rs`, `benches/encode_decode.rs`)
- [x] **Encoder em streaming** — *completo em v1.6* (`Encoder<W: Write>` / `Encoder<W: Write + Seek>`, contraparte simétrica do `Decoder<R: Read>` da v1.5)
- [x] **Quantização de paleta k-means** — *completo em v1.7* (`PaletteAlgorithm::KMeans`, algoritmo de Lloyd determinístico)
- [x] **Tone-mapping inverso no encode (SDR→HDR)** — *completo em v1.8* (`EncodeOptions::inverse_tonemap`, `--inverse-tonemap reinhard`)
- [x] **Decodificação em streaming com tiling 2D (`iDIM`)** — *completo em v1.9* (`Decoder<R>::next_tile()` agora transmite tiles reais `(x, y, width, height)` para arquivos `iDIM`; entrelaçamento continua sendo uma limitação de design permanente)
- [x] **Codificação em streaming com tiling 2D (`iDIM`)** — *completo em v1.10* (`Encoder<W>::add_idim_tile()`, contraparte simétrica da decodificação em streaming da v1.9; row-major e Z-order ambos suportados; `auto_dictionary`/paleta indexada/entrelaçamento continuam sendo limitações permanentes do `Encoder<W>`)
- [x] **Codificação em streaming com entrelaçamento par/ímpar** — *completo em v1.11* (`Encoder<W>::add_even_odd_rows()`; revisita o agrupamento da v1.9.1 de Adam7 e par/ímpar como uma única limitação — os passes do par/ímpar são reconstruíveis independentemente por linha, ao contrário do Adam7; `auto_dictionary`/paleta indexada/Adam7 continuam sendo limitações permanentes do `Encoder<W>`)

---

## 📈 Roadmap

| Versão | Recursos | Status |
|--------|----------|--------|
| **v1.0** | Chunks críticos, ZSTD, 14 filtros, metadados (EXIF/JSON/ICC/XMP/HDR), zDIC, sample_format float/half, segurança | ✅ Completo |
| **v1.1** | Filtros 14-15: TR-Directional (WebP Predictor 10) e Weighted adaptativo (inspirado no JPEG-XL) — 16 preditores no total; heurística MSAD; tiling 2D real (iDIM) com round-trip end-to-end; byte-shuffle encode/decode; **otimização AVX2 SIMD (Filtros 1-3)**; HDR tone-mapping | ✅ Completo |
| **v1.2** | **Aceleração SIMD Agressiva (AVX2 x86_64)**: Pack/Unpack 1/2/4-bit (8-16x), Expansão/redução de amostras (4-6x), Byte-shuffle com blocking (10-20%), Filter 3 melhorado (4-6x); **252 testes** (197 unit + 6 integration roundtrip + 49 SIMD); **Zero TODOs/FIXMEs**; Benchmarks Criterion; Feature-gated SIMD com detecção de CPU | ✅ Completo |
| **v1.2.1** | Refinamentos e despachante de operador para seleção de tone-mapping | ✅ Completo |
| **v1.3** | **NEON SIMD ARM (aarch64)**: todos os 14 filtros vetorizados portados para NEON, dispatch em compile-time, sem checagem de feature em runtime (NEON é baseline no ARMv8-A) | ✅ Completo (Filtros 1-14) |
| **v1.4** | **NEON SIMD ARM estendido a todos os módulos restantes**: pack/unpack, conversão de amostras, byte-shuffle, quantização de paleta — nenhum módulo SIMD é mais exclusivo de AVX2 | ✅ Completo |
| **v1.4.1** | **Validação real de execução ARM (emulação QEMU)**: suíte de testes completa rodada nativamente em aarch64 pela primeira vez — encontrado e corrigido um bug real de cálculo de índice no NEON que apenas checagem de cross-compile não conseguiria detectar | ✅ Completo |
| **v1.4.2** | **CI: verificação de cross-compile ARM64** — novo job `aarch64-cross-compile` roda `cargo check`/`clippy --target aarch64-unknown-linux-gnu` em cada push/PR, evitando que futuras regressões em aarch64 passem despercebidas | ✅ Completo |
| **v1.5** | **Auditoria focada em compressão (5 itens)**: filtro preditivo por linha (`Filter method=3`), benchmarks de compressão reais + gate de regressão no CI, garantia de não-regressão do `auto_dictionary`, quantização de paleta perceptualmente ponderada (distância redmean), investigação de retuning do `DEFAULT_TILE_ROWS` (mantido em 64, trade-off documentado) | ✅ Completo |
| **v1.6** | **Encoder em Streaming** (`Encoder<W: Write>` / `Encoder<W: Write + Seek>`): escreve `IHDR` + chunks ancilares + `IDAT`s row-strip incrementalmente à medida que os tiles chegam, contraparte simétrica do `Decoder<R: Read>` da v1.5; `finish()` define um `compression_method` conservador para destinos apenas `Write`, `finish_exact()` corrige para o valor exato (idêntico byte a byte a `encode()`) quando `W` também suporta `Seek` | ✅ Completo |
| **v1.6.1** | **CLI**: `cafe-encode` ganha as flags `--icc-profile-file`/`--xmp-file`, fechando uma lacuna de paridade de CLI para `EncodeOptions::icc_profile`/`xmp_metadata` | ✅ Completo |
| **v1.6.2** | **CLI + `compression_stats` real**: `DecodeResult::compression_stats` agora populado de verdade (tamanhos originais/comprimidos por chunk) em vez de sempre `None`; `cafe-decode` ganha `--show-stats` além de `--save-exif`/`--save-icc-profile`/`--save-xmp`/`--save-zstd-dict` para exportar metadados embutidos para arquivos separados | ✅ Completo |
| **v1.6.3** | **CI: workflow noturno de fuzzing** — novo `.github/workflows/fuzz.yml` roda `decode_fuzz`/`chunk_roundtrip_fuzz` por uma hora completa todas as noites (mais sob demanda via `workflow_dispatch`), separado do teste-fumaça de 60s por push já existente no `ci.yml` | ✅ Completo |
| **v1.7** | **`PaletteAlgorithm::KMeans`**: novo algoritmo de paleta indexada implementando o algoritmo de Lloyd (inicialização determinística via median-cut, sem dependência de RNG), tipicamente o menor erro quadrático médio entre os quatro algoritmos ao custo computacional mais alto — `--palette-algorithm kmeans` | ✅ Completo |
| **v1.8** | **Tone-mapping inverso no encode (síntese SDR→HDR)**: `EncodeOptions::inverse_tonemap`/`--inverse-tonemap reinhard` opt-in sintetiza dados HDR float a partir de entrada SDR (só Reinhard, sem inversa em forma fechada para Filmic); requer `sample_format=float` + `chdr_transfer` linear + RGBA | ✅ Completo |
| **v1.9** | **Suporte de `Decoder<R>::next_tile()` a tiling 2D (`iDIM`)**: gera um `Tile` por `IDAT` com sua posição real `(x, y, width, height)` na grade de tiles (row-major ou Z-order, incluindo tiles parciais de borda) em vez de sempre falhar para arquivos `iDIM`; entrelaçamento (Adam7/par-ímpar) continua sendo uma limitação de design permanente e documentada do `next_tile()` | ✅ Completo |
| **v1.9.1** | **Somente documentação**: as limitações do `Encoder<W>` com `auto_dictionary`/paleta indexada/entrelaçamento foram reclassificadas de "lacuna da v1" para limitação de design permanente e investigada — sem mudança de código ou comportamento | ✅ Completo |
| **v1.9.2** | **CI: job de teste ARM64 nativo** — novo job `arm64-native-test` roda a suíte de testes completa nativamente em `ubuntu-24.04-arm` (CPUs Arm de servidor reais, não x86_64 com emulação), fechando a lacuna entre a checagem de cross-compile já existente e a validação manual única via QEMU da v1.4.1 | ✅ Completo |
| **v1.9.3** | **Somente documentação**: semântica de `compression_method` esclarecida na especificação — nova nota normativa afirma que `bit0` é uma declaração de capacidade de limite inferior obrigatório, nunca um registro por chunk (papel exclusivo do `Flag`); nova nota de conformidade documenta que o decoder de referência não valida cruzadamente os dois | ✅ Completo |
| **v1.10** | **`Encoder<W>::add_idim_tile()`** — suporte a tiling 2D (`iDIM`) no encoder em streaming, contraparte simétrica da decodificação em streaming da v1.9; corrige a classificação da v1.9.1 do `iDIM` como limitação permanente do `Encoder<W>` (só é preciso saber a geometria de antemão, não os pixels); novo campo `EncoderOptions::idim: Option<(u16, u16, u8)>` (padrão `None`, não é breaking change), row-major e Z-order ambos suportados, `add_tile()`/`add_idim_tile()` mutuamente exclusivos | ✅ Completo |
| **v1.11** | **`Encoder<W>::add_even_odd_rows()`** — suporte a entrelaçamento par/ímpar (`Interlace = 2`) no encoder em streaming; revisita o agrupamento da v1.9.1 de Adam7 e par/ímpar como uma única limitação permanente — os dois passes do par/ímpar são reconstruíveis independentemente a partir de qualquer subconjunto de linhas, ao contrário do Adam7, que genuinamente não pode ser transmitido; novo campo `EncoderOptions::even_odd_interlace: bool` (padrão `false`, não é breaking change, mutuamente exclusivo com `idim`/`use_filter_per_row`/`use_byte_shuffle`), restrito a RGBA uint de 8 bits; `add_even_odd_rows()` aceita faixas contíguas de linhas de altura arbitrária e pode dividir um passe em múltiplos `IDAT`s; Adam7 continua permanentemente não suportado pelo `Encoder<W>` | ✅ Completo |
| **Futuro** | Validação real em hardware ARM físico de *usuário final* (Raspberry Pi, mobile, Apple Silicon), compressores adicionais, seleção de operador de tone-mapping via CLI para PQ/HLG/sRGB e Filmic no encode | 🔮 Planejado |

---

## 🐛 Reportar Issues

1. Verificar [issues existentes](../../issues)
2. Se novo: fornecer
   - Versão do CAFE
   - Arquivo de teste (se possível)
   - Stack trace completo
   - Sistema operacional / Rust version

Para vulnerabilidades de segurança: ver [SECURITY.md](docs/SECURITY_AUDIT.md)

---

## 👨‍💻 Autor

**Daniel Secco** — Criador e mantenedor  
Arquitetura, especificação, implementação de referência em Rust (v1.1)

---

## 🙏 Agradecimentos

- **ZSTD** (Yann Collet) — Algoritmo de compressão
- **PNG** (grupo de trabalho W3C) — Inspiração de design
- **Rust community** — Excelente linguagem e ferramentas

---

**Última atualização**: 2026-09-04 (v1.11: codificação em streaming com entrelaçamento par/ímpar — `Encoder<W>::add_even_odd_rows()` suporta `Interlace = 2`, revisitando o agrupamento da v1.9.1 de Adam7/par-ímpar como uma única limitação já que os passes do par/ímpar são reconstruíveis independentemente por linha; campo opt-in `EncoderOptions::even_odd_interlace`, mutuamente exclusivo com `add_tile()`/`add_idim_tile()`; Adam7 continua permanentemente não suportado; v1.10: codificação em streaming com tiling 2D — `Encoder<W>::add_idim_tile()` agora suporta arquivos `iDIM` (row-major e Z-order), contraparte simétrica da decodificação em streaming da v1.9; campo opt-in `EncoderOptions::idim`, `add_tile()`/`add_idim_tile()` mutuamente exclusivos; v1.9.3: semântica de `compression_method` esclarecida na especificação — definição normativa de "limite inferior obrigatório" e nota de conformidade do decoder adicionadas à seção 4.1, somente documentação, sem mudança de código; v1.9.2: CI ganha um job `arm64-native-test` rodando a suíte de testes completa nativamente em `ubuntu-24.04-arm` (silício ARM64 real, não emulado); v1.9.1: limitações do `Encoder<W>` com `auto_dictionary`/paleta indexada/entrelaçamento reclassificadas de "lacuna da v1" para limitação de design permanente e investigada — somente documentação, sem mudança de código; v1.9: decodificação em streaming com tiling 2D — `Decoder<R>::next_tile()` agora suporta arquivos `iDIM`, transmitindo tiles reais `(x, y, width, height)` em vez de falhar; entrelaçamento continua sendo uma limitação documentada permanente; v1.8: tone-mapping inverso no encode — `EncodeOptions::inverse_tonemap`, `--inverse-tonemap reinhard`, síntese SDR→HDR; v1.7: `PaletteAlgorithm::KMeans` — quantização de paleta k-means determinística, `--palette-algorithm kmeans`; v1.6.3: workflow noturno de fuzzing no CI — `.github/workflows/fuzz.yml`; v1.6.2: rastreamento real de `DecodeResult::compression_stats` + `cafe-decode` ganha `--show-stats`/`--save-exif`/`--save-icc-profile`/`--save-xmp`/`--save-zstd-dict`; v1.6.1: `cafe-encode` ganha as flags `--icc-profile-file`/`--xmp-file`; v1.6: encoder em streaming — `Encoder<W: Write>` / `Encoder<W: Write + Seek>`, contraparte simétrica do `Decoder<R: Read>` da v1.5)  
**Cobertura de testes**: 332 testes de lib + 13 suítes de teste de integração (roundtrip, encoder em streaming com 46 testes incluindo tiling 2D e entrelaçamento par/ímpar, SIMD, regressão de compressão, regressão de dicionário, algoritmo de paleta, benchmarks de tile_rows, etc.)  
**Próxima revisão de segurança**: 2027-08-04
