# Snapgroup: Documento de Arquitetura (Snapper-Less)

## 1. O Ponto de Viragem: Remoção do Snapper
O `snapgroup` deixará de ser um *wrapper* do `snapper` para se tornar um gestor nativo de BTRFS, focado em performance (Zero-Cost), código procedural limpo e integração profunda com o sistema Arch/CachyOS.
- **Vantagem:** Execução em milissegundos via chamadas diretas às `btrfs-progs`.
- **Armazenamento:** Estrutura simples de diretórios (`/.snapshots_snapg/`) com ficheiros de metadados JSON (`info.json`) dentro de cada subvolume para evitar base de dados centralizada.

## 2. Gestão de Boot (A Abordagem de Dupla Opção)
O instalador/configurador inicial (`snapg init`) perguntará ao utilizador qual arquitetura de `/boot` ele possui ou deseja usar:

### Opção A: Modo Nativo (BTRFS /boot) - *Recomendado*
O Kernel e o Initramfs vivem na partição BTRFS. A partição FAT32 só é montada em `/efi` contendo o binário do bootloader.
- **Rollback:** 100% atómico e instantâneo. O rollback reverte sistema, kernel e drivers simultaneamente.
- **Risco:** Requer que o bootloader saiba ler BTRFS (GRUB nativo, Limine com driver).

### Opção B: Modo Híbrido (FAT32 /boot) - *Compatibilidade Legada*
O sistema mantém o Kernel em `/boot` (FAT32).
- **Rollback:** O `snapgroup` orquestra o rollback BTRFS e *imediatamente* sincroniza/copia o Kernel correspondente para a FAT32.
- **Custo:** Snapshots demoram mais tempo devido à cópia física do kernel para um histórico na FAT32.
- **Vantagem:** Compatibilidade "out-of-the-box" com qualquer distro e Secure Boot padrão.

## 3. Funcionalidades Planeadas

### 3.1. Rastreio do Pacman (`snapg pacman` ou `snapg list --pacman`)
- **Mecânica:** O hook do pacman invoca `snapg create --trigger pacman`.
- **Como funciona:** O `snapgroup` guarda essa tag no metadado do snapshot. Na hora de listar, é um simples `if meta.trigger == "pacman"` para filtrar, permitindo ao utilizador ver exatamente onde o sistema foi alterado pelo gestor de pacotes.

### 3.2. Snapshots Baseados em Tempo e Eventos
- **Eventos:** Um systemd service do tipo `Type=oneshot` com `WantedBy=multi-user.target` invoca `snapg create --trigger boot` a cada arranque.
- **Tempo:** Um `systemd timer` invoca `snapg create --trigger cron` de hora a hora ou diariamente.
- **Independência:** O `snapgroup` gere a sua própria limpeza (Prune) analisando as datas no metadado para não lotar o disco.

### 3.3. Snapgroup Explorer (`snapg explore <id>`)
- **Mecânica:** Monta/abre diretamente o caminho estático do snapshot (`/.snapshots_snapg/<id>/snapshot`) no gestor de ficheiros padrão do utilizador (`xdg-open`).
- **Vantagem:** Permite resgatar um único ficheiro corrompido sem precisar de reverter todo o sistema operativo.

## 4. O Script de Migração (Análise de Viabilidade)
*Em discussão.* O script para converter FAT32 /boot para BTRFS /boot.
