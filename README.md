# <img src="./assets/cafe_logo.png" alt="CAFÉ: Compression Adaptative Filtering Experiment" style="vertical-align: bottom"/> CAFE Format

### Compression Adaptive Filtering Experiment

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Status: Experimental](https://img.shields.io/badge/Status-Experimental-orange)](https://github.com/yourusername/cafe-format)
[![C Language](https://img.shields.io/badge/Language-C-blue.svg)](<https://en.wikipedia.org/wiki/C_(programming_language)>)

> **An experimental image format designed for machine learning pipelines and research**

**CAFE** (Compression Adaptive Filtering Experiment) is a block-based, lossless image format built from the ground up for modern computational workflows. Born from a Master's thesis in Artificial Intelligence, it addresses the I/O bottlenecks that plague large-scale computer vision tasks.

## 🔍 What Problem Does CAFE Solve?

Traditional image formats were designed for display, not computation. When training ML models:

- **Thousands of tiny files** create filesystem overhead
- **Serial loading** limits data throughput
- **Metadata is separate** from image data
- **No native GPU acceleration** for decompression

CAFE reimagines image storage for the age of machine learning by packaging entire datasets in a single, optimized container.

## 🏗️ Core Architecture

CAFE is built around simple but powerful principles:

### **Block-Based Design**

```text
# Each image is divided into independent 128×128 blocks

[█████████] [█████████]
[ Block 1 ] [ Block 2 ] # Decode in parallel
[█████████] [█████████]
```

### **Intelligent Compression**

- **Primary codec**: Zstandard (ZSTD) for speed/ratio balance
- **Secondary**: Finite State Entropy (FSE) for low-entropy data
- **Optional**: Predictors to reduce spatial redundancy
- **No compression**: When counterproductive

### **AI-Ready Metadata**

Store embeddings, labels, segmentation masks, and features directly alongside the pixels they describe.

## ⚙️ Technical Specifications

| Feature            | Implementation             | Benefit            |
| ------------------ | -------------------------- | ------------------ |
| **Block Size**     | 128×128 pixels             | Parallel decoding  |
| **Compression**    | ZSTD + FSE                 | Fast decompression |
| **Color Depth**    | Up to 16-bit/channel       | Scientific imaging |
| **Alpha Channel**  | Supported in all modes     | Transparency       |
| **Error Recovery** | Per-block CRC + delimiters | Robustness         |
| **Streaming**      | Progressive decoding       | Web/remote viewing |

## 🚧 Current Status

**Important**: This is a solo research project implemented gradually alongside academic work. The format is evolving, and not all planned features are yet implemented.

### **Implemented** (v0.4-alpha)

- ✅ Basic block structure and header
- ✅ ZSTD compression/decompression
- ✅ C reference implementation
- ✅ Simple predictor (differential coding)
- ✅ CRC-32 checksums

### **In Progress**

- 🚧 GPU acceleration (CUDA/nvCOMP integration)
- 🚧 FSE codec implementation
- 🚧 Benchmark suite
- 🚧 Python bindings via CFFI

### **Planned**

- 📋 AI metadata chunk format
- 📋 Hierarchical/progressive decoding
- 📋 Thumbnail generation
- 📋 Full format specification document

## 📦 Getting Started

### Building from Source

````bash
# Clone the repository
git clone https://github.com/yourusername/cafe-format.git
cd cafe-format

# Build the C library
make

# Install system-wide (optional)
sudo make install

### Basic Usage in C

```c
#include <cafe.h>

// Open a CAFE file
cafe_file_t* file = cafe_open("dataset.cafe", "rb");

// Read header information
cafe_header_t header;
cafe_read_header(file, &header);

// Access blocks independently
for (int i = 0; i < header.num_blocks; i++) {
    cafe_block_t block;
    cafe_read_block(file, &block, i);
    process_block(&block);
}

cafe_close(file);
````

### For ML Pipelines (Experimental)

The reference C implementation provides:

- Streaming API for progressive loading
- Batch decompression for GPU acceleration
- Memory-mapped I/O support
- Thread-safe block access

Future Python bindings will provide PyTorch/TensorFlow DataLoader compatibility.

## 🏃‍♂️ Performance Goals

| **Scenario**                 | **Target Improvement**            |
| ---------------------------- | --------------------------------- |
| Batch loading (1000+ images) | 3-5× faster than PNG              |
| GPU decompression            | 10× faster than CPU               |
| Storage efficiency           | 10-30% better than PNG            |
| Memory usage                 | 50% reduction vs individual files |

_These are research targets, not guarantees._

## 🧠 The Research Angle

CAFE is more than just a file format—it's a testbed for ideas:

1.  **How much can we speed up ML training by optimizing the data layer?**
2.  **Can intelligent compression reduce cloud training costs?**
3.  **What metadata should live with images in AI workflows?**

This project explores the intersection of data formats, compression algorithms, and machine learning systems.

## 🤔 Why "CAFE"?

**Pronounced "kah-FEH"** (like coffee in Portuguese/Brazilian), the name reflects the project's philosophy beyond its acronym (Compression Adaptive Filtering Experiment):

- **Rich and complex** (like a well-crafted espresso)
- **Meant to fuel work** (especially during long training sessions)
- **Better shared** (open source, like coffee shared among colleagues)
- **Universally appealing** (transcends language barriers)

Just as coffee shops have historically been hubs of intellectual exchange, CAFE aims to be a meeting point for ideas in compression, computer vision, and machine learning systems.

## 🛣️ Roadmap

**Phase 1: Core Format (Current)**

- Stable specification v0.4
- Reference implementations in C and Python
- Basic benchmarks

**Phase 2: Acceleration**

- GPU decompression with nvCOMP
- PyTorch/TensorFlow integration
- WebAssembly decoder for browsers

**Phase 3: Advanced Features**

- AI-powered predictors
- Dataset versioning within files
- Federated learning support

## 🤝 Contributing & Feedback

This is primarily a research codebase maintained by a single developer (as part of a Master's thesis). However:

- **Issues are welcome** for bugs, suggestions, or discussions
- **Pull requests will be considered**, especially for bug fixes
- **Research collaborations** are encouraged

If you're working on related problems (ML systems, compression, efficient I/O), feel free to reach out!

## 📚 Academic Context

CAFE is being developed as part of a Master's thesis in Artificial Intelligence. The goal is to demonstrate that thoughtful data format design can significantly impact ML system performance.

**Planned Publications**

1.  Format specification and design rationale
2.  Performance evaluation against traditional formats
3.  Case study: Impact on real-world training pipelines

## 📄 License

CAFE Format is licensed under the MIT License:

```text
MIT License

Copyright (c) 2026 Daniel Secco

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

See LICENSE file for the complete text.

## 📚 Third-Party Licenses

CAFE uses the following third-party libraries:

### Zstandard (ZSTD)

- License: BSD 3-Clause License / GPLv2 (dual licensed)
- Usage: Primary compression/decompression engine
- Source: <https://github.com/facebook/zstd>
- Copyright: Facebook, Inc. and affiliates

### Finite State Entropy (FSE)

- License: BSD 2-Clause License
- Usage: Secondary compression for low-entropy data
- Source: <https://github.com/Cyan4973/FiniteStateEntropy>
- Copyright: Yann Collet

## 📝 Citation

If you use or reference CAFE in academic work:

```bibtex
@mastersthesis{cafe2026,
	title={CAFE: An Image Format Optimized for Machine Learning Pipelines},
	author=Daniel Secco Ferreira e Silva,
	year={2026},
	school={American Global Tech University},
	note={Compression Adaptive Filtering Experiment}
}
```

# 💭 Final Thoughts

In an era where we spend thousands of GPU-hours training models, we often overlook the simplest optimization: how we store and retrieve the data itself. CAFE is one attempt to fix that.

_"Good data formats are invisible; great ones make your entire pipeline faster."_

**Maintainer**: Daniel Secco  
**Status**: Active development (as thesis permits)  
**Contact**: daniel.secco@computer.org  
**Repository**: <https://github.com/seccofs/cafe>

☕ _Last updated: January 2025_

