# Restore Safety Follow-ups

Fila técnica para os próximos ciclos de segurança do `snapg restore`.

Este documento não é uma ADR. Ele registra trabalho pendente, riscos e critérios
de verificação. Decisões de política, especialmente sobre initramfs em `/boot`
FAT32, devem virar ADR quando forem fechadas.

## Estado atual

`v0.2.1-beta` fechou o batch de guardas de segurança do restore:

- preflight de filesystem único em `save` e `restore`;
- preservação do regret anterior como aside durante restore checkpoint
  (`aside_existing_regrets`, `src/rollback.rs:107`);
- lock global exclusivo em `save`, `restore` e `delete` (`src/lock.rs`).

Com isso, o restore ficou protegido contra três classes de falha:

- operar configs Snapper que vivem fora do filesystem Btrfs de `/`;
- perder o regret anterior antes de saber se o novo rollback commitou;
- duas instâncias mutantes colidirem nos nomes fixos de subvolume e no mount
  temporário em `/run/snapgroup/{uuid}`.

## Próximo ciclo: boot safety follow-ups

Os itens 1 e 2 são correções pontuais de poucas linhas, sem decisão de design
pendente — entram juntos numa branch curta e mergeiam rápido. O item 3 carrega
uma decisão e só deve ser implementado depois dela resolvida (já encaminhada
abaixo).

### 1. Scan de `/boot` sem seguir symlink

Local: `scan_boot_dir`, `src/boot.rs:265`.

Risco:

- `path.is_dir()` segue symlink. Um symlink dentro de `/boot` apontando para
  fora pode expandir o scan recursivo para uma árvore inesperada.

Correção:

- trocar a decisão de diretório para `entry.file_type()?.is_dir()`;
- ignorar symlinks explicitamente.

Precedente: o próprio módulo já usa `entry.file_type()?.is_dir()` em
`src/boot.rs:223` e `:383`. A linha 265 é a exceção destoante — a correção
alinha com o padrão existente, não introduz um novo.

Verificação:

- teste unitário ou fixture temporário com symlink dentro de uma árvore de
  boot falsa;
- `cargo test --locked`;
- `cargo clippy --locked --all-targets -- -D warnings`.

### 2. Checar exit status do `systemctl reboot -i`

Local: `prompt_reboot`, `src/commands.rs:760` (chamada em `:769`).

Risco:

- chamar `.status()?` valida apenas falha ao executar o binário. Exit code
  não-zero do `systemctl` pode ser tratado como sucesso pelo `snapg`.

Correção:

- capturar `status`;
- se `!status.success()`, retornar erro explícito.

Verificação:

- teste focado se a chamada for extraída para helper;
- inspeção de caminho de erro;
- `cargo test --locked`;
- `cargo clippy --locked --all-targets -- -D warnings`.

### 3. Regra de lock para `boot-clean`

Local: `boot_clean`, `src/commands.rs:642`.

Risco:

- `boot-clean` também deleta subvolumes, mas roda no boot e tem semântica
  diferente dos comandos interativos.

Opções consideradas:

- incluir `boot-clean` no lock global atual;
- criar lock próprio para cleanup pós-boot;
- manter fora do lock e documentar o motivo.

Decisão: **lock próprio para o cleanup pós-boot.** É a única opção que satisfaz
as duas restrições simultâneas:

- o lock global travaria `boot-clean` se um `restore`/`delete` interativo
  ficasse pendurado segurando o lock — inaceitável no caminho de boot;
- ficar totalmente sem lock permitiria `boot-clean` apagar subvolumes enquanto
  outro comando manipula o mesmo top-level.

Um lock dedicado (ex: `/run/snapgroup-boot-clean.lock`) protege contra duas
execuções de cleanup concorrentes sem acoplar ao lock interativo. Falta validar
se cleanup e `restore`/`delete` podem coexistir no mesmo top-level com locks
separados, ou se ainda assim precisam serializar.

Verificação:

- simular segunda instância segurando o lock;
- garantir que a escolha não deixa `boot-clean` apagar subvolumes enquanto
  outro comando manipula o mesmo top-level.

## Ciclo seguinte: initramfs fingerprint

Local do gate atual: `boot_matches_snapshot`, `src/boot.rs:139` (chamado por
`sync_fat32`, `src/boot.rs:33`).

Problema:

- em `/boot` FAT32, o gate atual compara `vmlinuz`, mas não prova que o
  initramfs corresponde aos inputs do snapshot restaurado;
- se o kernel for o mesmo, mudanças em `mkinitcpio.conf`, hooks ou arquivos
  incluídos podem deixar um initramfs antigo em `/boot`.

Opções:

- regenerar initramfs sempre que `/boot` for FAT32;
- calcular fingerprint dos inputs do `mkinitcpio` e regenerar quando mudar;
- manter o comportamento atual e documentar a limitação como aceita.

Decisão pendente (vira ADR ao fechar):

- definir se o custo de regenerar sempre é aceitável;
- se houver fingerprint, definir exatamente quais inputs entram na assinatura
  e onde ela fica registrada.

Verificação:

- restore cross-version de kernel;
- restore same-kernel com `mkinitcpio.conf` alterado;
- falha induzida de `mkinitcpio` deve restaurar o backup de `/boot`
  (`backup_boot_files`/`restore_backup`, `src/boot.rs`) e bloquear o reboot.

## Recuperação de aside órfão

Estado atual:

- se existir `<subvol>.snapgroup_regret_aside`, o restore aborta
  (`aside_existing_regrets`, `src/rollback.rs:107`);
- isso evita sobrescrever um regret antigo preservado por uma tentativa
  interrompida.

Melhoria futura:

- melhorar a UX de recuperação manual;
- mostrar por config:
  - path do aside;
  - path do regret canônico;
  - se o slot canônico está livre ou ocupado;
  - comando sugerido para restaurar ou descartar.

Opções:

- apenas melhorar a mensagem de erro;
- criar comando dedicado, como `snapg doctor`;
- criar comando dedicado, como `snapg recover-aside`.

Verificação:

- fixture manual com aside órfão;
- confirmar que o comando nunca restaura automaticamente quando o slot canônico
  está ocupado.

## Fora de escopo por enquanto

- performance de `list_groups()` com histórico grande;
- suporte real a múltiplos filesystems Btrfs;
- empacotamento com artefato binário anexado em GitHub Release.
