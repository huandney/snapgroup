# AGENTS.md — snapgroup (`snapg`)

Fatos estáveis deste repositório, para qualquer agente que programe nele (Claude, Codex, Antigravity…).
O método de trabalho geral vem da Coding Doctrine do usuário (`~/Projetos/coding-doctrine`); aqui ficam só os fatos deste projeto.
O estado do trabalho em andamento fica em `STATUS.md` na raiz (local, fora do git via `.git/info/exclude`).

**Este projeto mexe no boot e nos subvolumes do sistema do usuário.** Um restore interrompido já travou o boot real dele (incidentes em `docs/incidents/`). Na dúvida, pare e pergunte.

## O que é

Wrapper do Snapper em Rust (binário `snapg`, crate `snapgroup`, beta) que tira snapshots agrupados de todas as configs do Snapper e restaura o grupo inteiro de forma transacional: checkpoints manuais e do pacman, Regret (desfazer o restore), lixeira com purge, sincronização do `/boot` em FAT32 (Limine) e conferência do boot antes de reiniciar. Interface e README em pt-BR.

## Testes

```sh
cargo test
cargo clippy --all-targets
```

- Em 2026-11-20 (v0.15.0-beta): 150 testes ok, 5 ignorados; clippy sem avisos. Os dois limpos antes de cada commit; relate as contagens.
- Os ignorados precisam de root: quatro pedem um Btrfs vazio montado em `SNAPG_TEST_BTRFS` (`cargo test --no-run && sudo SNAPG_TEST_BTRFS=/mnt/x <binário de teste> --ignored`), um confere o boot do sistema real. Rodar como root exige aceite.
- Telas (TUI): confira com o binário compilado num pty, em 80 e 120 colunas, e diga quais telas e teclas passou.
- Todo `snapg` se reexecuta com `sudo`. Somente leitura, sem pedir: `snapg list`, `snapg log`, `snapg boot-check`. Todo o resto escreve: `save`, `restore`, `rename`, `delete`, `trash`, `setup`, `pacman on|off`, `doctor` (pode sincronizar o `/boot`).

## Sistema real

- Instalação: `makepkg -si` na raiz (PKGBUILD local; os `.pkg.tar.zst` de builds anteriores ficam na raiz, fora do git). Instalar troca os hooks do pacman (`00-snapgroup-pending`, `05-snapgroup-pre`, `zz-snapgroup-boot`) e o `/usr/bin/snapg` que roda na próxima transação: **exige aceite aqui**, ao contrário do padrão da doutrina. Diga qual build ficou instalado (`pacman -Q snapgroup`) e como voltar (`sudo pacman -U snapgroup-<versão anterior>-x86_64.pkg.tar.zst`).
- Nunca reinicie a máquina nem rode `restore`/`doctor` sem aceite. Comando que pede reboot fica com o usuário.
- O que um build de branch muda só aparece de verdade dentro de um pacman real (hook) ou num restore real: diga quando um comportamento não foi visto lá.

## Dados reais

- Subvolumes e snapshots do Btrfs, o `/boot` (FAT32, Limine, hashes BLAKE2B no `limine.conf`), `/etc/snapgroup.conf` (o usuário edita; `config::set` preserva comentários) e o estado em `/var/lib/snapgroup/`.
- Locks: `/run/snapgroup.lock` (uma instância mutante por vez) e o mutex do Limine em `/run/lock/boot-partition.lock`.

## Convenções

- Git: `main` estável, `develop` integração, branches de `develop`; push após cada commit (remoto `huandney/snapgroup`). Review `high` para o que toca boot, sinais, locks ou subvolumes.
- **Versão beta:** manifestos com versão plain (`Cargo.toml`, `Cargo.lock`, `PKGBUILD` com `pkgver=X.Y.0`); tag anotada `vX.Y.0-beta`, cabeçalho do CHANGELOG `## [X.Y.0-beta]`, GitHub Release **prerelease** com título `vX.Y.0-beta` e assets `snapg-x86_64` + `snapgroup-X.Y.0-1-x86_64.pkg.tar.zst`. O sufixo da interface vem de `DISPLAY_VERSION` em `src/ui/term.rs`; ao sair do beta, tire o `-beta` dali.
- `CHANGELOG.md` em inglês, sob `[Unreleased]`, agrupado por componente (Restore, Hook…).
- `docs/`: `architecture/` (decisões; correção de ADR datada, original mantido), `proposals/` (Status/Data/Relacionado/TL;DR), `incidents/<data>-<slug>/` (postmortem + logs brutos), `roadmap/`, `guides/`.

## Armadilhas

- O Limine 12 lê só FAT e ISO9660: `/boot` em Btrfs exigiria trocar de bootloader.
- O Limine confere o BLAKE2B de cada arquivo citado no `limine.conf`: `vmlinuz` igual ao do root não basta para dizer que o boot está bom.
- O `limine-snapper-sync` guarda só as 8 entradas mais novas no histórico; um par `vmlinuz`+`initramfs` ocupa ~330 MB por kernel (initramfs com Nvidia ~311 MB) e o `mkinitcpio` leva 13–16 s por kernel.
- O `/boot` deve ser conferido contra o root que vai **bootar** (o `subvol=` da entrada do Limine), não contra o root que recebeu o sync: foi o incidente de junho.
- Ctrl-C do terminal vai para o grupo de processos inteiro: filhos dentro da janela crítica do sync rodam em grupo próprio com stdin de `/dev/null`.
- Decisões firmes, só revisitar com motivo forte: membros de um grupo são pareados pelo label do grupo (nunca `@` de um momento com `@home` de outro); o restore prepara todos os membros antes do primeiro rename (falta de espaço aborta sem tocar no sistema vivo); a retenção é do snapg, por grupo, nunca o `--cleanup-algorithm` do Snapper (podaria grupos pela metade).
