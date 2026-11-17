# snapgroup

Wrapper para [Snapper](http://snapper.io/) que agrupa snapshots de múltiplos subvolumes Btrfs (ex: `/` e `/home`) num único ID lógico — `save`, `restore` e `delete` operam no grupo inteiro de uma vez.

Binário: `snapg`.

## Por que existe

Snapper trata cada config (`root`, `home`, ...) como universos independentes. Se você quer reverter `/` e `/home` juntos pra um ponto coerente no tempo, tem que correr atrás dos números na unha. `snapgroup` resolve isso amarrando os snapshots via `userdata` (`snapgroup-id=<epoch>`) e oferecendo rollback transacional pareado.

## Requisitos

- Btrfs como filesystem raiz
- Snapper instalado e com pelo menos uma config criada (`snapper -c <nome> create-config <path>`)
- Layout de subvolumes "Snapper-style" — ex: `@`, `@home`, com `.snapshots` montado em cada subvol ativo (padrão openSUSE / CachyOS / instaladores Arch modernos)
- Todas as configs Snapper no **mesmo** filesystem Btrfs que `/` (configs em outro Btrfs são rejeitadas por ora)

## Instalação (Arch / pacman)

Build local do PKGBUILD:

```sh
git clone git@github.com:huandney/snapgroup.git
cd snapgroup
makepkg -si
```

## Comandos

| Comando | O que faz |
|---|---|
| `snapg save [descrição]` | Cria snapshot em todas as configs Snapper, agrupado num ID |
| `snapg list` | Lista grupos existentes (mais recente primeiro) e o Regret ativo, se houver |
| `snapg restore` | Restauração interativa via TUI: escolhe um checkpoint **ou** o Regret; Regret pendente desfaz sem comando separado |
| `snapg delete [-y]` | Apaga checkpoints (TUI multi-seleção; `-y` apaga o mais recente sem perguntar) |
| `snapg pacman [on\|off]` | Escolhe quem faz os checkpoints das transações do pacman: o snapg (`on`) ou o snap-pac (`off`). Sem argumento, mostra o atual |
| `snapg log` | Mostra o que cada checkpoint do pacman instalou, atualizou ou removeu |
| `snapg setup` | Ajustes numa tela só: quem faz os checkpoints do pacman, quantos manter, dias na lixeira e o filtro do restore. Cada mudança é gravada na hora |

Todos os comandos pedem `sudo` automaticamente (re-exec via `sudo` se não for root). Apenas uma instância mutante (`save`/`restore`/`delete`) roda por vez — um lock global (`flock` em `/run/snapgroup.lock`) bloqueia execuções concorrentes.

## Checkpoints do pacman

Com `snapg pacman on`, toda transação do pacman ganha antes um checkpoint do snapg: todas as configs do Snapper no mesmo grupo, como num `snapg save`. Uma execução do `paru`, `yay` ou `makepkg -si` (o paru faz um `-Syu` e um `-U` por pacote do AUR) vira um checkpoint só; um `pacman` direto, um por transação. O nome diz o que vem depois do snapshot: `Antes de paru -Syu`, `Antes de pacman -U google-chrome`.

- **Ligar e desligar.** O snap-pac e o snapgroup ficam os dois instalados. `on` desliga os hooks do snap-pac com links para `/dev/null` em `/etc/pacman.d/hooks` e liga o do snapg; `off` faz o inverso. Instalar o snapgroup não muda nada até o `on`, um upgrade mantém a escolha, e desinstalar o snapgroup devolve os checkpoints ao snap-pac.
- **Retenção.** `KEEP_PACMAN` (padrão 20, em `/etc/snapgroup.conf`) é uma fila só dos checkpoints do pacman, separada do `KEEP_GROUPS`. Os além dos 20 mais novos vão para a lixeira (`TRASH_PRUNE_DAYS`). Renomear um deles o fixa. Eles não descartam o Regret.
- **O que cada um fez.** `snapg log` lê o `pacman.log` (no subvolume `@log`, que sobrevive aos restores) e mostra os pacotes de cada checkpoint, separados entre o que veio do repositório e o que foi compilado (AUR).
- **Antes de restaurar.** O `restore` compara o banco de pacotes guardado no snapshot do `/` com o do sistema atual e mostra quantos pacotes mudam, os de boot primeiro, e avisa quando o próprio snapgroup voltaria de versão. No seletor, Tab filtra entre Todos, Manuais e Pacman; `RESTORE_FILTER` escolhe o filtro inicial.
- **O hook nunca aborta uma transação.** Se um hook do snap-pac ainda estiver ativo, o snapg recua para não dobrar os snapshots; com uma restauração pendente de reboot ou outro `snapg` aberto (espera até 10 s), a transação segue sem checkpoint e o motivo aparece na saída do pacman.

### Migração do snap-pac

Depois do `snapg pacman on`, os pre/post que o snap-pac já tinha criado continuam no Snapper: o cleanup `number` só apaga o que passa do `NUMBER_LIMIT`, e o snapg ignora snapshots sem grupo. Para listar quantos são:

```sh
for c in $(sudo snapper --csvout list-configs --columns config | tail -n +2); do
  printf '%s: ' "$c"
  sudo snapper -c "$c" --csvout list --columns number,type,cleanup | awk -F, '$2 != "single" && $3 == "number"' | wc -l
done
```

Para apagá-los (irreversível):

```sh
for c in $(sudo snapper --csvout list-configs --columns config | tail -n +2); do
  sudo snapper -c "$c" --csvout list --columns number,type,cleanup |
    awk -F, '$2 != "single" && $3 == "number" {print $1}' |
    xargs -r sudo snapper -c "$c" delete
done
```

O `/etc/snap-pac.ini` não é tocado: com `snapg pacman off` o snap-pac volta com a mesma configuração.

## Como funciona o `restore` (rollback transacional)

`restore` abre uma TUI com os checkpoints disponíveis e, se existir, o **Regret** (estado anterior à última restauração). Ao restaurar um checkpoint, para cada membro do grupo, em duas fases:

**Fase 1 — preparação (não toca em nada vivo):**
1. Cria cópia writable do snapshot RO (`btrfs subvolume snapshot`) num nome intermediário (`<subvol>.snapgroup_prep`). É a etapa cara (cópia de metadata, propensa a ENOSPC); se falhar, todos os preps são apagados e o sistema vivo fica 100% intocado.

**Fase 2 — commit (só renames, atômico por membro):**
2. Renomeia o subvol ativo → `<subvol>_snapg_regret` (rename é metadata-only; mounts existentes sobrevivem por inode)
3. Renomeia a cópia writable → nome ativo original
4. Move `.snapshots` aninhado de volta pro novo subvol ativo

Se a fase 2 falhar no meio de um grupo, o rollback dos membros já feitos é revertido (automaticamente ou com confirmação). Depois de um restore aplicado, um novo checkpoint só pode ser restaurado após reboot ou revertendo a restauração pendente (selecionando a opção do Regret no próprio menu de restore); isso evita empilhar restaurações sobre um estado pendente.

## O `Regret` (botão de arrependimento)

Cada `restore` arquiva o sistema que você está deixando como `<subvol>_snapg_regret`. Ele aparece na TUI do próximo `restore` como **⟲ Estado Anterior à Restauração** e pode ser restaurado pra desfazer a última restauração. Semântica Highlander: existe **um** Regret por vez — um novo `save` descarta o atual (os checkpoints do pacman não).

Se o Regret representa uma restauração pendente de reboot, selecioná-lo no `snapg restore` desfaz essa restauração por baixo dos panos e apaga o subvol recém-restaurado imediatamente. Se o Regret já foi efetivado por reboot, restaurá-lo troca de volta para o estado anterior e os subvols vivos pré-restauração viram `<subvol>_snapg_discard_<label>` (não dá pra deletar enquanto montados). O cleanup é automático via "serviço fantasma":

1. Após restaurar o Regret, snapg roda `systemctl enable snapg-cleanup.service` no rootfs restaurado.
2. No próximo boot, o systemd executa o serviço, que chama `snapg boot-clean`: apaga todos os `<subvol>_snapg_discard_*` e em seguida roda `systemctl disable snapg-cleanup.service`.
3. O serviço fica inerte de novo até o próximo restore de Regret. Zero overhead em boots normais.

## `/boot` em FAT32 (Limine e afins)

Quando `/boot` está em FAT32 (fora do snapshot Btrfs), kernel e initramfs não viajam com o rollback. Nesse caso o `restore` sincroniza `/boot` com o snapshot restaurado (copia o `vmlinuz`, regenera o initramfs a partir do root restaurado, re-injeta os hashes BLAKE2B no `limine.conf`). Se a sincronização falhar ou ficar dessincronizada, o reboot é **bloqueado** com instrução de recuperação — bootar ali cairia em Emergency Mode.

Limitações desse sync:

- **UKI não é suportada.** Se `/boot` tem uma UKI (`EFI/Linux/*.efi`), restores que trocam o `/` são recusados antes de qualquer alteração.
- **O initramfs é regenerado com as ferramentas do sistema atual.** Só o `mkinitcpio.conf` e os módulos do kernel vêm do root restaurado; hooks, binários e opções de preset são os do sistema em execução.
- **Subvolumes aninhados acompanham o sistema ativo.** O que vive em subvolume próprio dentro de `@`/`@home` (ex: `/var/lib/machines`, camadas do Docker) fica fora dos snapshots e não é revertido pelo `restore`. Entre o restore e o reboot esses caminhos ficam vazios no sistema em execução; se algo for recriado ali, é preservado como `<nome>.snapg-displaced`.

## Status

Beta. Sem testes de integração ainda — valide o ciclo `save` → `restore` → restaurar o Regret num ambiente descartável antes de confiar como botão de pânico.

## Licença

MIT.
