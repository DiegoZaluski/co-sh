# cosh

[English](../../README.md) | [Português](./README.pt-BR.md) | [Español](./README.es.md) | **Русский** | [中文](./README.zh-CN.md) | [日本語](./README.ja.md)

Cosh — универсальный coding agent для терминала с инструментами разработки ПО и *computer tools*, позволяющими взаимодействовать с графическими приложениями.

<img width="560" height="315" alt="demo" src="https://github.com/user-attachments/assets/d746684d-ecc1-4ce7-ad0c-a0b18d9892b8" />

Основные возможности:

- **Computer tools через accessibility tree** — агент читает графические интерфейсы и работает с ними через компактное структурированное представление, а к скриншотам прибегает только как к запасному варианту. Меньше обработанных изображений — меньше расход токенов.
- **TUI, написанная с нуля на Rust** (Ratatui), вдохновлённая UX OpenCode. Настройка провайдеров, моделей и режимов работы выполняется прямо в интерфейсе — почти никакой ручной конфигурации.
- **Независимые крейты** — инструменты разработки вынесены в крейты, не зависящие от приложения, и могут использоваться как библиотеки за пределами Cosh.

## Установка

Протестировано на Linux и Windows. macOS пока не тестировалась — если что-то не сработает, [создайте issue](https://github.com/DiegoZaluski/co-sh/issues) с максимально подробным описанием.

**Linux, macOS, Windows (Git Bash / WSL):**

```sh
curl -fsSL https://raw.githubusercontent.com/DiegoZaluski/co-sh/main/download.sh | bash
```

Необязательные переменные: `COSH_BIN_DIR` (каталог установки), `COSH_VERSION` (конкретная версия) и `COSH_VARIANT` (`slim` — по умолчанию, или `slim-embed` — с локальным RAG через fastembed).

После установки запустите `cosh` в терминале. Если каталог установки отсутствует в вашем `PATH`, скрипт сам выведет команду для его добавления. Если возникнут сложности с установкой — создайте issue, буду рад помочь!

## Крейты

| Крейт | Описание |
|---|---|
| [`cosh-tools`](../../crates/cosh-tools) | Инструменты разработки и computer tools |
| [`cosh-tui`](../../crates/cosh-tui/README.md) | Библиотека TUI-виджетов (рендеринг markdown/diff, layout) — компоненты, на которых построен терминальный интерфейс приложения |
| [`cosh-sdk`](../../crates/cosh-sdk) | SDK для создания агентов на базе cosh |
| [`cosh-recall`](../../crates/cosh-recall) | Память и восстановление контекста |
| [`cosh-onnx`](../../crates/cosh-onnx/README.md) | Движок принятия решений ONNX, включаемый feature `onnx` |

## Планы

- **Движок решений Laya/ONNX** — необязательная feature `onnx`, предоставляемая crate `cosh-onnx`; см. [руководство crate](../onnx/onnx.md).
- **Оркестрация harness'ов через граф** — новый режим сессии для координации нескольких harness'ов.

## Лицензия

Apache-2.0.
