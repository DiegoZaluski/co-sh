# cosh

[English](../../README.md) | **Português** | [Español](./README.es.md) | [Русский](./README.ru.md) | [中文](./README.zh-CN.md) | [日本語](./README.ja.md)

Cosh é um coding agent de propósito geral para o terminal, com ferramentas de engenharia de software e *computer tools* que permitem interagir com aplicações gráficas.

<img width="560" height="315" alt="demo" src="https://github.com/user-attachments/assets/d746684d-ecc1-4ce7-ad0c-a0b18d9892b8" />

Destaques:

- **Computer tools via accessibility tree** — o agente lê e opera interfaces gráficas por uma representação estruturada e compacta, e só recorre a screenshots como fallback. Menos imagens processadas, menos tokens consumidos.
- **TUI construída do zero em Rust** (Ratatui), inspirada na UX do OpenCode. Configuração de providers, modelos e modos de operação direto na interface — quase zero configuração manual.
- **Crates independentes** — as ferramentas de desenvolvimento vivem em crates desacoplados da aplicação, reutilizáveis como bibliotecas fora do Cosh.

## Instalação

Testado em Linux e Windows. macOS não foi testado ainda — se algo falhar, [abra uma issue](https://github.com/DiegoZaluski/cosh/issues) com o máximo de detalhes.

**Linux, macOS, Windows (Git Bash / WSL):**

```sh
curl -fsSL https://raw.githubusercontent.com/DiegoZaluski/cosh/main/download.sh | bash
```

Variáveis opcionais: `COSH_BIN_DIR` (diretório de instalação), `COSH_VERSION` (versão específica) e `COSH_VARIANT` (`slim`, padrão, ou `slim-embed`, com RAG local via fastembed).

Depois de instalar, rode `cosh` no terminal. Se o diretório de instalação não estiver no seu `PATH`, o próprio script mostra o comando para adicioná-lo. Qualquer dificuldade na instalação, abra uma issue — ficarei feliz em ajudar!

## Crates

| Crate | Descrição |
|---|---|
| [`cosh-tools`](../../crates/cosh-tools) | Ferramentas de desenvolvimento e computer tools |
| [`cosh-tui`](../../crates/cosh-tui) | Interface de terminal (Ratatui) |
| [`cosh-sdk`](../../crates/cosh-sdk) | SDK para construir agentes sobre o cosh |
| [`cosh-recall`](../../crates/cosh-recall) | Memória e recall de contexto |

## Roadmap

- **Laya** — pequeno modelo de decisão para auxiliar o harness, distribuído como feature em versão separada.
- **Orquestração de harness em grafo** — um novo modo de sessão para coordenar múltiplos harnesses.

## Licença

Apache-2.0.
