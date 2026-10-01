# cosh

[English](../../README.md) | [Português](./README.pt-BR.md) | **Español** | [Русский](./README.ru.md) | [中文](./README.zh-CN.md) | [日本語](./README.ja.md)

Cosh es un coding agent de propósito general para la terminal, con herramientas de ingeniería de software y *computer tools* que le permiten interactuar con aplicaciones gráficas.

<img width="560" height="315" alt="demo" src="https://github.com/user-attachments/assets/d746684d-ecc1-4ce7-ad0c-a0b18d9892b8" />

Aspectos destacados:

- **Computer tools vía accessibility tree** — el agente lee y opera interfaces gráficas mediante una representación estructurada y compacta, y solo recurre a capturas de pantalla como respaldo. Menos imágenes procesadas, menos tokens consumidos.
- **TUI construida desde cero en Rust** (Ratatui), inspirada en la UX de OpenCode. La configuración de providers, modelos y modos de operación se hace directamente en la interfaz — casi cero configuración manual.
- **Crates independientes** — las herramientas de desarrollo viven en crates desacoplados de la aplicación, reutilizables como bibliotecas fuera de Cosh.

## Instalación

Probado en Linux y Windows. macOS aún no ha sido probado — si algo falla, [abre un issue](https://github.com/DiegoZaluski/cosh/issues) con el mayor detalle posible.

**Linux, macOS, Windows (Git Bash / WSL):**

```sh
curl -fsSL https://raw.githubusercontent.com/DiegoZaluski/cosh/main/download.sh | bash
```

Variables opcionales: `COSH_BIN_DIR` (directorio de instalación), `COSH_VERSION` (versión específica) y `COSH_VARIANT` (`slim`, predeterminado, o `slim-embed`, con RAG local vía fastembed).

Después de instalar, ejecuta `cosh` en la terminal. Si el directorio de instalación no está en tu `PATH`, el propio script muestra el comando para agregarlo. Ante cualquier dificultad en la instalación, abre un issue — ¡con gusto te ayudaré!

## Crates

| Crate | Descripción |
|---|---|
| [`cosh-tools`](../../crates/cosh-tools) | Herramientas de desarrollo y computer tools |
| [`cosh-tui`](../../crates/cosh-tui) | Interfaz de terminal (Ratatui) |
| [`cosh-sdk`](../../crates/cosh-sdk) | SDK para construir agentes sobre cosh |
| [`cosh-recall`](../../crates/cosh-recall) | Memoria y recall de contexto |

## Roadmap

- **Laya** — pequeño modelo de decisión para asistir el harness, distribuido como feature en una versión separada.
- **Orquestación de harnesses en grafo** — un nuevo modo de sesión para coordinar múltiples harnesses.

## Licencia

Apache-2.0.
