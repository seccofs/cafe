# CAFE — Compression Adaptative Filtering Experiment

[![License](https://img.shields.io/badge/license-BSD--3--Clause%20OR%20GPL--2.0-blue)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.70%2B-orange)](https://www.rust-lang.org)
[![Build Status](https://img.shields.io/badge/build-passing-brightgreen)]()
[![Security](https://img.shields.io/badge/security-audited-green)](docs/SECURITY_AUDIT.md)

Um formato de imagem moderno baseado em chunks, inspirado em PNG, com suporte a compressão ZSTD, filtros preditivos avançados (16 tipos), paleta indexada, metadados estruturados (EXIF, JSON, ICC, XMP) e entrelaçamento progressivo.

**Versão**: 1.1.0  
**Status**: ✅ Completo e auditado  
**Compatibilidade**: Rust 2021+

---

## 🚀 Características Principais

### Compressão Inteligente
- **ZSTD** com fallback para dados brutos (seção 3.2)
- Nível ajustável (1-22)
- Dicionário ZSTD opcional (`zDIC` chunk)

### Filtros Preditivos Avançados
- **16 tipos de filtros**: None, Sub, Up, Average, Paeth, MED, Gradient, Simple Median, 2nd Order, 4-way Directional (4 variantes), Context-Based, TR-Directional (WebP Predictor 10) e Weighted adaptativo (inspirado no JPEG-XL)
- Aplicados por bloco (tile) para máxima eficiência
- Seleção automática por heurística: **Entropia de Shannon** (padrão), **MSAD** (`--filter-heuristic msad`) ou **compressão de teste real** (`--filter-heuristic test`), que comprime cada preditor candidato e escolhe o de menor tamanho final

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

### Segurança
- ✅ Proteção contra decompression bomb (CWE-409)
- ✅ Validação de input não confiável
- ✅ Sem panic em arquivos malformados/truncados
- ✅ [Auditoria completa](docs/SECURITY_AUDIT.md)

---

## 📦 Estrutura do Projeto

```
cafe/
├── src/                           # Biblioteca principal
│   ├── cafe.rs                    # Núcleo: encode/decode, chunks (re-exports)
│   ├── constants.rs               # Assinatura, flags, color types, filtros
│   ├── chunk.rs                   # Framing de chunks (Length/Type/Flag/Data/CRC32)
│   ├── codec.rs                   # Compressão ZSTD com fallback (seção 3.2)
│   ├── color.rs                   # Conversões de cor, pack/unpack, float/half
│   ├── filter.rs                  # 16 filtros preditivos + heurísticas
│   ├── interlace.rs               # Adam7 e par/ímpar
│   ├── types.rs                   # EncodeOptions, iDim, cHDR, Palette, etc.
│   └── error.rs                   # CafeError
├── tools/                         # Ferramentas CLI
│   ├── cafe-encode.rs            # Encoder binário
│   └── cafe-decode.rs            # Decoder binário
├── docs/                          # Documentação
│   ├── CAFE-spec.md              # Especificação completa (v1.1, 566 linhas)
│   ├── SECURITY_AUDIT.md         # Auditoria de segurança
│   └── DEVELOPER_GUIDE.md        # Guia para desenvolvedores
├── tests/                         # Testes de integração e round-trip
├── examples/                      # Exemplos de uso
├── Cargo.toml                     # Dependências e configuração
├── Cargo.lock                     # Lock de versões
├── README.md                      # Este arquivo
├── LICENSE                        # Dual license (BSD-3 OR GPL-2)
└── .github/
    └── workflows/                 # CI (build, clippy -D warnings, fmt, doc)
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
# Build release (otimizado)
cargo build --release

# Executáveis
./target/release/cafe-encode input.png output.cafe
./target/release/cafe-decode output.cafe decoded.png
```

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

### Compressão
- **PNG típico**: 100 KB → 60-80 KB (CAFE, 20-40% ganho)
- **Imagem colorida**: Melhor em dados com padrões (gradientes, linhas)
- **Imagem com ruído**: Similar a PNG (pouco ganho de filtro)

### Velocidade
- **Encode**: ~100 MP/s (Ryzen 5, release mode)
- **Decode**: ~150 MP/s
- **Nível 19 (padrão)**: ~2-5% mais lento que PNG

---

## 🔒 Segurança

- ✅ **Auditado**: [Relatório completo](docs/SECURITY_AUDIT.md)
- ✅ **Padronizado**: Segue boas práticas de formato de imagem
- ✅ **Sem panics**: Todas as falhas retornam `Result`, nunca crash em input não confiável
- ✅ **Limite de memória**: Proteção contra decompression bomb (1 GiB/chunk)

---

## 📋 Dependências

```toml
image = "0.24"          # Leitura/escrita de PNG, JPEG, etc.
zstd = "0.13"           # Compressão ZSTD
serde_json = "1.0"      # Parsing JSON
half = "2.4"            # Half-float (fp16)
crc32fast = "1.3"       # CRC32 para chunks
```

---

## 📚 Documentação

- **[CAFE Specification](docs/CAFE-spec.md)** — Especificação completa (566 linhas)
- **[Security Audit](docs/SECURITY_AUDIT.md)** — Auditoria de segurança detalhada
- **[Developer Guide](docs/DEVELOPER_GUIDE.md)** — Guia técnico para contribuidores
- **[API Docs](https://docs.rs/cafe)** — Documentação Rust (gerada por `cargo doc`)

---

## 📝 Licença

Dual license: **BSD-3-Clause OR GPL-2.0-or-later**

Mesma abordagem do ZSTD — choose the license that works best for you.

---

## 🤝 Contribuições

Contribuições são bem-vindas! Áreas com potencial:

- [ ] Paleta indexada com k-means
- [ ] Dicionário ZSTD automático
- [ ] SIMD nos filtros e no empacotamento sub-byte
- [ ] Byte-shuffle (Filter method=1)
- [ ] Testes de fuzzing
- [ ] Benchmarking vs PNG, WebP, JPEG-XL

---

## 📈 Roadmap

| Versão | Recursos | Status |
|--------|----------|--------|
| **v1.0** | Chunks críticos, ZSTD, 14 filtros, metadados (EXIF/JSON/ICC/XMP/HDR), zDIC, sample_format float/half, segurança | ✅ Completo |
| **v1.1** | Filtros 14-15: TR-Directional (WebP Predictor 10) e Weighted adaptativo (inspirado no JPEG-XL) — 16 preditores no total; heurística MSAD; tiling 2D real (iDIM) com round-trip end-to-end | ✅ Completo |
| **Futuro** | Byte-shuffle, compressores adicionais, SIMD, progressivo melhorado | 🔮 Planejado |

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

**Última atualização**: 2026-08-05  
**Próxima revisão de segurança**: 2027-08-04
