# Proposta revisada: `doctor` reparável para boot sync interrompido

## Status

Proposto, após revisão crítica de três análises.

## Contexto

Sistemas com `/boot` em FAT32 continuam sendo o ponto frágil do restore:

- o rollback BTRFS troca subvolumes de forma quase atômica;
- `/boot` fica fora do snapshot;
- kernel, initramfs e hashes do Limine precisam ser sincronizados depois do
  rollback;
- se faltar energia ou o processo for interrompido entre essas etapas, o root
  restaurado e o `/boot` podem ficar incompatíveis.

Artefato relacionado:

- `docs/incidents/2026-06-02-fat32-interrupted-boot-sync/boot-emergency.log`

O log mostra um boot que chegou ao Emergency Mode com kernel
`7.0.10-2-cachyos`, enquanto o root restaurado não tinha módulos compatíveis:

```text
ConditionFileNotEmpty=/lib/modules/7.0.10-2-cachyos/modules.devname
mount: /boot: tipo de sistema de arquivos desconhecido "vfat"
Failed to mount /boot
Reached target Emergency Mode
```

O erro de `vfat` é sintoma: o kernel carregado pelo bootloader não encontra seus
próprios módulos em `/usr/lib/modules/<kver>`, então nem o módulo de filesystem
consegue ser carregado.

## Síntese das análises

A proposta inicial de criar `.snapg_boot_sync.pending` +
`.snapg_boot_sync.json` estava na direção certa, mas prometia demais:

- o JSON não prova que o initramfs é correto; no máximo registra que o `snapg`
  terminou uma execução;
- o `initramfs` não é artefato determinístico fácil de verificar contra o
  snapshot, porque é gerado por `mkinitcpio`;
- o diretório `.snapg_boot_backup` já existe e é um ótimo sinal de sync iniciado
  e não fechado limpo;
- se `.snapg_boot_backup` existir, o sync de reparo precisa pular o gate
  `boot_matches_snapshot`; caso contrário, um `vmlinuz` já copiado pode fazer o
  sync retornar cedo sem regenerar o initramfs nem remover o backup;
- em FAT32, qualquer marcador precisa ser tratado como frágil: JSON ausente,
  inválido, corrompido ou com hash divergente nunca autoriza confiança;
- rodar `doctor` no emergency shell só funciona se o sistema conseguir acessar
  `/boot` ou se o usuário informar/montar o alvo com `--boot`. Se o kernel não
  tiver módulo `vfat`, o caminho correto é Live ISO ou rescue externo;
- restauração para root antigo pode deixar o usuário com um `/usr/bin/snapg`
  velho. Recuperação realmente robusta exige, no futuro, um binário/script fora
  do root restaurável.

## Decisão de design

Separar três conceitos:

1. **Sinal de interrupção:** `.snapg_boot_backup` remanescente.
2. **Verificação real atual:** comparação byte-a-byte do `vmlinuz` em `/boot`
   contra o root alvo.
3. **Recibo de conclusão:** `.snapg_boot_sync.json`, opcional na primeira fase,
   útil para UX/diagnóstico, mas não tratado como prova absoluta de correção.

Isso evita duplicar estado transacional e evita transformar ausência de JSON em
alarme permanente em sistemas saudáveis que nunca passaram por restore do
`snapg`.

## Regra revisada do `doctor`

Para `/boot` não-FAT32:

- manter estado `NativeBoot`.

Para `/boot` FAT32:

- `NeedsSync` se o `vmlinuz` divergir do root alvo;
- `NeedsSync` se `.snapg_boot_backup` existir;
- `NeedsSync` se existir `.snapg_boot_sync.json`, mas ele for inválido,
  corrompido ou incompatível com o kernel/vmlinuz atual;
- `Synced` se o `vmlinuz` casar e não houver sinal de operação interrompida;
- `Unverified` pode ser usado, se quisermos expor nuance: vmlinuz casa, mas não
  há recibo recente do `snapg` sobre initramfs.

Primeira implementação recomendada: **não introduzir `Unverified` no fluxo
principal ainda**. Mostrar `Synced` quando o `vmlinuz` casa e não há sinal de
interrupção mantém compatibilidade e evita falso positivo permanente.

O ponto cego do initramfs será mitigado de forma operacional: se o sync foi
interrompido, `.snapg_boot_backup` denuncia. Quando esse sinal existir, a
correção deve forçar o sync completo, sem o early-return de
`boot_matches_snapshot`, porque o caso perigoso é exatamente `vmlinuz` já
copiado e initramfs ainda antigo.

## UX do `snapg doctor`

`snapg doctor` deve analisar e oferecer correção quando encontrar um estado
reparável.

```text
 SnapGroup 0.4.0-beta
 ▪ Diagnóstico de boot

   Alvo · sistema atual
   ├─ root           /
   ├─ boot           /boot
   ├─ filesystem     vfat
   ├─ kernel groups  1
   ├─ initramfs      1
   └─ estado         sincronização incompleta

   motivo: backup de boot remanescente
   ação sugerida: sincronizar /boot com /

   Aplicar correção agora?
   > Sim
     Não
```

Se o usuário confirmar, o `doctor` chama o mesmo painel técnico usado pelo
restore:

```text
 SnapGroup 0.4.0-beta
 ▪ Sincronização de boot

   etapa 1 de 5 · criando backup
   ├─ backup       em execução
   ├─ vmlinuz      aguardando
   ├─ initramfs    aguardando
   ├─ limine.conf  aguardando
   └─ estado       aguardando

   executando
   backup /boot -> /boot/.snapg_boot_backup
```

Depois, o `doctor` reavalia e mostra o diagnóstico final.

## Emergency shell

O `doctor` deve tentar ser útil no emergency shell, mas precisa falhar bem.

`findmnt --target /boot` sozinho não basta: se `/boot` não estiver montado, ele
pode retornar o filesystem pai e fazer o `doctor` acreditar que o boot é nativo.
Para diagnosticar esse caso, o `doctor` precisa de outra fonte:

- `--boot` explícito;
- `/etc/fstab` do root alvo;
- `findmnt --verify`;
- ou `blkid`/UUID do dispositivo esperado.

Se o alvo esperado for FAT32 e a montagem falhar com falta de suporte a `vfat`
(`unknown filesystem type "vfat"`), a mensagem deve ser específica:

```text
Não foi possível montar /boot (FAT32).

O kernel carregado não parece ter módulos compatíveis no root restaurado, então
o módulo vfat não pode ser carregado.

Recuperação:
1. dê boot por uma Live ISO;
2. monte o root BTRFS em /mnt;
3. monte a partição FAT32 em /mnt/boot;
4. rode: snapg doctor --root /mnt --boot /mnt/boot --apply
```

Isso evita esconder o problema atrás de erro genérico de mount.

## Recibo `.snapg_boot_sync.json`

O recibo continua útil, mas com escopo honesto:

- ele registra que uma execução do `snapg` terminou;
- ajuda o `doctor` a explicar o estado;
- pode ajudar logs e suporte;
- não substitui a comparação real do `vmlinuz`;
- não prova deterministicamente todos os inputs do `mkinitcpio`.

Conteúdo sugerido, se implementado:

```json
{
  "version": 1,
  "snapg_version": "0.4.0-beta",
  "kernel": "7.0.10-2-cachyos",
  "vmlinuz_blake2b": "...",
  "initramfs_blake2b": [
    {
      "path": "0262533c4ca04359a5f379b8e3f83042/linux-cachyos/initramfs-linux-cachyos",
      "hash": "..."
    }
  ],
  "limine_conf_blake2b": "...",
  "completed_at_unix": 1780000000
}
```

Se o recibo existir, o `doctor` deve parsear de forma fail-safe:

- erro de leitura → `NeedsSync`;
- JSON inválido → `NeedsSync`;
- UTF-8 inválido → `NeedsSync`;
- hash divergente entre recibo e arquivo atual em disco → `NeedsSync`;
- versão desconhecida → `NeedsSync` ou aviso conservador.

O hash do initramfs no recibo só detecta corrupção ou alteração posterior do
arquivo gerado. Ele não prova que o initramfs corresponde deterministicamente ao
snapshot, porque o `mkinitcpio` não gera um artefato reprodutível simples.

Se o recibo não existir, isso **não** deve virar `NeedsSync` automaticamente.
Ausência de recibo é normal em sistemas que nunca passaram por essa versão do
`snapg`.

## Durabilidade em FAT32

Se escrevermos `.snapg_boot_sync.json`, a implementação deve tratar persistência
como parte do contrato:

1. escrever em arquivo temporário;
2. `fsync` do arquivo temporário;
3. `rename` para o caminho final;
4. `fsync` do diretório, quando suportado;
5. só depois remover `.snapg_boot_backup`;
6. remover backup também com melhor esforço de flush do diretório.

Mesmo assim FAT32 não tem journaling. Por isso o marcador nunca deve ser a única
fonte de verdade. Qualquer leitura suspeita vira caminho de correção, não
caminho de confiança.

## Plano incremental

### Fase 1: `doctor` detecta interrupção real

- Tratar `.snapg_boot_backup` existente como `NeedsSync`.
- Mostrar motivo claro: `backup de boot remanescente`.
- `snapg doctor` pergunta `Aplicar correção agora?`.
- `snapg doctor --apply` executa sem pergunta.
- A correção chamada pelo `doctor` deve forçar sync completo quando
  `.snapg_boot_backup` existir:
  - pular o gate `boot_matches_snapshot`;
  - regenerar initramfs;
  - atualizar `limine.conf`;
  - verificar o estado final;
  - remover `.snapg_boot_backup`.
- Falha ao remover `.snapg_boot_backup` deixa de ser silenciosa. Como backup
  remanescente vira sinal de `NeedsSync`, uma remoção que falha deve gerar
  aviso ou erro claro.
- Se `/boot` não montar por `vfat`, mostrar instrução de Live ISO.

Essa fase é pequena e resolve diretamente o cenário de queda/interrupção.

### Fase 2: recibo de conclusão

- Adicionar `.snapg_boot_sync.json` como recibo, não como prova absoluta.
- Incluir hash de `vmlinuz`, hash de cada initramfs gerado e hash do
  `limine.conf`.
- Escrever com temp+rename+fsync.
- Validar fail-safe quando o recibo existir.
- Não tratar recibo ausente como erro global.

### Fase 3: endurecer janela crítica

Quando os testes de reprodução terminarem:

- adiar ou bloquear `Ctrl+C` durante a janela de sync;
- manter o gate de reboot em desync;
- considerar `systemd-inhibit` para impedir reboot/shutdown enquanto `/boot`
  está sendo escrito.

### Fase 4: rescue fora do root restaurável

Para fechar o problema de root antigo com `snapg` antigo:

- estudar cópia atual de `snapg` em local fora do root restaurável;
- estudar script/binário de rescue em `/boot`;
- estudar initramfs/entrada rescue dedicada.

Isso é maior e não bloqueia a correção curta.

## Alternativas

### Regenerar initramfs sempre

Mais simples, mas caro. Pode ser útil como modo conservador:

```bash
snapg doctor --apply --force-boot-sync
```

Não substitui o sinal de interrupção, porque queda de energia ainda precisa ser
detectada.

### Inspecionar initramfs

Frágil. O `initramfs` é artefato gerado e não há assinatura nativa simples que
prove todos os inputs do `mkinitcpio`.

### `/boot` em BTRFS

Solução arquitetural correta. Kernel, initramfs e módulos voltam juntos no
snapshot. Remove a classe inteira, mas exige migração de arquitetura de boot.

## Recomendação atual

Implementar primeiro a Fase 1.

Motivo:

- menor mudança;
- usa estado que já existe;
- resolve queda/interrupção com evidência concreta;
- não adiciona promessa falsa sobre initramfs;
- mantém o `doctor` útil e interativo.

Depois implementar a Fase 2 se o recibo trouxer valor real para UX, logs e
diagnóstico.

## Prompt para validação externa

Use o texto abaixo para pedir revisão crítica a outra IA.

```text
Estou desenvolvendo o snapgroup, uma ferramenta Rust que agrupa snapshots do
Snapper/BTRFS e faz restore pareado de subvolumes como root e home.

Contexto técnico:
- O root fica em BTRFS.
- Em alguns sistemas, /boot fica em FAT32 separado.
- Ao restaurar um snapshot BTRFS antigo, o root volta atomicamente, mas /boot
  fica fora do snapshot.
- Para evitar boot quebrado, o snapgroup sincroniza /boot após o rollback:
  copia vmlinuz do root restaurado, regenera initramfs com mkinitcpio e atualiza
  hashes BLAKE2B do Limine.
- O sync atual já cria /boot/.snapg_boot_backup antes de mexer nos artefatos e
  remove esse backup apenas depois do sync/verificação final.
- Em queda de energia, esse backup pode ficar para trás.
- O doctor atual compara vmlinuz byte a byte entre /boot e o root alvo. Isso
  detecta kernel mismatch, mas não prova sozinho que initramfs foi regenerado.

Incidentes:
1. Se faltar energia ou o processo for interrompido durante o sync de /boot, o
   sistema pode cair em Emergency Mode no boot seguinte. O journal mostra kernel
   7.0.10-2 carregado, mas /lib/modules/7.0.10-2 ausente no root restaurado,
   seguido de "mount: /boot: tipo de sistema de arquivos desconhecido vfat".
2. Se vmlinuz já foi copiado, mas initramfs não foi gerado, o doctor pode
   declarar "coerente" se olhar só para vmlinuz.

Proposta revisada:
- Fase 1: fazer snapg doctor tratar /boot/.snapg_boot_backup remanescente como
  NeedsSync, mostrar "backup de boot remanescente" e perguntar "Aplicar correção
  agora?".
- Quando /boot/.snapg_boot_backup existir, a correção deve forçar sync completo:
  pular o gate boot_matches_snapshot, regenerar initramfs, atualizar
  limine.conf, verificar e remover o backup. Sem isso, se vmlinuz já foi
  copiado, o sync pode retornar cedo e deixar initramfs antigo.
- Falha ao remover /boot/.snapg_boot_backup deve ser reportada, não engolida,
  porque backup remanescente passa a significar NeedsSync.
- snapg doctor --apply deve executar a correção sem pergunta.
- Se /boot não estiver montado, findmnt --target /boot pode enxergar o
  filesystem pai. Então doctor precisa usar --boot explícito, /etc/fstab,
  findmnt --verify ou blkid para saber que /boot deveria ser FAT32.
- Se o alvo esperado for FAT32 e a montagem falhar por "unknown filesystem type
  vfat", doctor deve explicar que o kernel carregado não tem módulos compatíveis
  no root restaurado e orientar Live ISO: montar root em /mnt, montar FAT32 em
  /mnt/boot e rodar snapg doctor --root /mnt --boot /mnt/boot --apply.
- Fase 2 opcional: adicionar /boot/.snapg_boot_sync.json como recibo de
  conclusão, não como prova absoluta. Se existir e estiver inválido, tratar como
  NeedsSync. Se não existir, não transformar isso em erro global.
- Hashes do recibo só comparam recibo vs arquivo atual em disco; eles detectam
  corrupção/alteração posterior, não provam relação determinística do initramfs
  com o snapshot.
- Se o JSON for escrito, usar temp+rename+fsync(file)+fsync(dir), porque FAT32
  não tem journaling.
- Manter a comparação real do vmlinuz como verificação principal.
- Depois dos testes de reprodução, bloquear ou adiar Ctrl+C/reboot durante a
  janela crítica de sync.
- Longo prazo: /boot em BTRFS ou rescue fora do root restaurável.

Pergunta:
Essa proposta revisada é a menor solução correta para recuperar quedas de
energia/interrupção via snapg doctor e reduzir o ponto cego do initramfs sem
criar falso positivo permanente em sistemas saudáveis? Há algum caso em que
.snapg_boot_backup remanescente não deve significar NeedsSync? O recibo JSON
traz valor suficiente para justificar a Fase 2, ou a Fase 1 + vmlinuz compare
bastam por enquanto?

Responda como revisão técnica: achados primeiro, riscos, recomendação e plano
incremental de implementação.
```

---

# Parte 2: `doctor` mira o subvolume que boota, não o `/` montado

## Status

Proposto, após incidente de 2026-06-02
(`docs/incidents/2026-06-02-fat32-interrupted-boot-sync/postmortem.md`).

## Contexto

A Parte 1 cobre `/boot` interrompido. Este incidente expôs uma falha
**independente e anterior**: o `snapg doctor`, rodado de dentro de um snapshot
de resgate, sincronizou o `/boot` para o subvolume montado em `/` (o snapshot
de resgate) em vez do `/@` que boota por padrão — e com isso *piorou* o estado,
mantendo o sistema em Emergency Mode.

Reconstrução do incidente:

1. `snapg restore` para um checkpoint antigo (kernel 7.0.3-1). Rollback comita:
   `/@` vira o root antigo (módulos 7.0.3-1).
2. O boot-sync pós-rollback é interrompido (Ctrl+C). `/boot` fica em 7.0.10-2.
3. Reboot → boot padrão (`subvol=/@`) carrega kernel 7.0.10-2 de `/boot` contra
   `/@` que só tem módulos 7.0.3-1 → Emergency Mode.
4. Usuário boota pelo Limine o snapshot `660` (kernel 7.0.10-2), que sobe.
5. Roda `snapg doctor`. O doctor usa `/` como alvo — mas `/` é o snapshot 660,
   **não** `/@`. Reporta NeedsSync e sincroniza `/boot` para 7.0.10-2 (o kernel
   do resgate).
6. Reboot → boot padrão (`/@`, 7.0.3-1) ainda não casa com `/boot` (7.0.10-2) →
   Emergency Mode de novo.

## Causa raiz

`snapg doctor` sem argumentos usa `/` como root alvo. Quando se boota um
snapshot de resgate pelo bootloader, `/ ≠ /@`: o doctor "conserta" o `/boot`
para o ambiente de resgate, não para o subvolume que boota por padrão.

Consequência: os mecanismos da Parte 1 (detecção de interrupção, sync forçado)
estão corretos, mas pressupõem que o doctor mira o root certo. Em cenário de
resgate, a **seleção de alvo** do doctor é o elo fraco, e precede a Parte 1.

## O sinal correto

O sistema expõe diretamente o que vai bootar, sem heurística:

```text
findmnt -no FSROOT /     ->  /@/.snapshots/660/snapshot   (subvol montado em /)
fstab, entrada "/"       ->  subvol=/@                     (subvol que boota)
```

Quando esses dois divergem, `/` não é o root que vai bootar. É o mesmo
princípio da Parte 1 (fstab como fonte da verdade), aplicado ao **root** em vez
do `/boot`.

## Regra de decisão

```text
fsroot_atual  = findmnt -no FSROOT /            // "@/.snapshots/660/snapshot"
subvol_padrao = subvol= da entrada "/" no fstab // "@"
normalizar ambos (remover "/" inicial)
se fsroot_atual == subvol_padrao -> boot normal: alvo "/" está correto (atual)
se divergem                      -> resgate: "/" é o root errado
```

A detecção guarda **apenas o caminho implícito** (`snapg doctor` sem args). O
caminho explícito (`--root`/`--boot`) faz bypass: quem passa root explícito
sabe o que está fazendo (inclusive preparar `/boot` para um snapshot de
propósito).

## Comportamento no resgate — duas opções

**Opção 1 (mínima, mais segura): detectar, recusar e instruir.** Ao divergir, o
doctor não sincroniza. Mostra o contexto e o comando exato (device de
`findmnt -no SOURCE /`):

```text
 ▪ Diagnóstico de boot

   Você está num snapshot de resgate.
   ├─ montado em /      @/.snapshots/660/snapshot
   ├─ boota por padrão  @  (conforme fstab)
   └─ risco             sincronizar /boot daqui miraria o root errado

   monte o root padrão e rode contra ele:
     mount -o subvol=/@ /dev/nvme0n1p4 /mnt/at
     snapg doctor --root /mnt/at --boot /boot --apply
```

Poucas linhas, zero efeito colateral, para o footgun na hora. Custo: montagem
manual.

**Opção 2 (auto-fix): montar o subvol padrão e mirar nele.** O doctor monta o
`@` **read-only** num temp (só lê módulos/vmlinuz/mkinitcpio.conf; o `/boot` já
está montado e é o único alvo de escrita), roda diagnose+sync, e desmonta com
cleanup garantido. A montagem ro é a segurança: não há como corromper o `@`.
Custo: lifecycle de mount/umount e descoberta de device, num estado frágil.

Recomendação: Opção 1 primeiro (corta o problema com risco quase nulo); Opção 2
como follow-up se quisermos o doctor autônomo no resgate. A detecção é a mesma.

## Backstop pareado: invariante final antes do reboot

Independente de como o mismatch surja, antes de qualquer "Reiniciar agora?",
checar se o subvolume que boota por padrão tem `/usr/lib/modules/<kver>` para o
kernel que está em `/boot`. Se não → aviso alto + bloqueio. Hoje o
`verify_synced` só compara contra o root que recebeu o sync, não contra o que
realmente boota. Esse cinto teria pego o estado do incidente (boot 7.0.10-2 vs
`/@` 7.0.3-1) em qualquer caminho.

## Fase 2 — menu unificado: doctor como resolvedor

A Fase 1 detecta o resgate e recusa+instrui (piso seguro). A Fase 2 fecha o
ciclo: o doctor deixa de só apontar o comando e passa a **resolver na hora**,
oferecendo as saídas num menu. A intenção é o doctor ser um resolvedor único —
o usuário em crise decide tudo num lugar só.

### A assimetria que o menu precisa respeitar

As duas saídas **não são a mesma operação**, e confundi-las recria o bug:

- **A — tornar bootável o sistema configurado (`/@`):** é um **sync de `/boot`**.
  Escreve `/boot` para casar com o kernel do `/@`. Não toca no root.
- **B — adotar outro sistema (snapshot bootado ou desfazer restore):** é um
  **rollback do `/@`** (restore). Mudar só o `/boot` para o kernel do snapshot
  de resgate deixaria o `/@` descasado → Emergency de novo. Para "ir ao kernel
  novo", o `/@` precisa mudar — território do `restore`, não do sync de `/boot`.

Por isso B nunca é "sincronizar `/boot` para o snapshot montado". B é sempre um
rollback do root.

### Estrutura do menu (default A)

```text
 ▪ Diagnóstico de boot
   ! você está num snapshot de resgate
   ├─ montado em /   @/.snapshots/660/snapshot   kernel 7.0.10-2
   ├─ boota          @                            kernel 7.0.3-1
   └─ /boot          kernel 7.0.10-2  (descasado do que boota)

   Como resolver?
 > Tornar o sistema padrão (@) bootável — sincroniza /boot p/ 7.0.3-1   [recomendado]
   Mudar o que boota — restaurar outro snapshot ou desfazer a última restauração
```

- **Default = A**, porque é **não-destrutiva** e **honra o restore que o usuário
  já fez** (faz o sistema bootar o que ele já está configurado para bootar). B
  descarta esse restore e exige escolha ativa.
- A linha mostra a **transição de kernel** (montado vs boota), tornando a
  escolha informada — sem isso o usuário não vê que é um downgrade/upgrade.

### Opção A — sync contra o `/@` (auto-mount)

O doctor monta o subvol padrão **read-only** num temp (lê módulos/vmlinuz/
mkinitcpio.conf de lá; `/boot` já está montado e é o único alvo de escrita),
roda o sync e desmonta com cleanup garantido. Read-only é a segurança: não há
como corromper o `/@`. É a Opção 2 da seção anterior, agora acionada pelo menu.

### Opção B — handoff para o `restore` (inclui o "cancelar restauração")

B delega ao fluxo de `snapg restore`, que **já** lista:

- os checkpoints (promover qualquer snapshot a sistema padrão), e
- o **Regret** (`RestorePlan::Regret` → `execute_restore_regret`): desfazer a
  última restauração, voltando ao estado exato anterior.

Ou seja, a ideia de "ao restaurar, ter opção de cancelar a restauração" **já
existe** como o Regret e é alcançada por B sem mecanismo novo no doctor. B =
"mudar o que boota" abre o picker do restore, e o Regret é um dos itens.

Promover o snapshot de resgate e desfazer o último restore são **paths
distintos** com resultados às vezes parecidos: o Regret volta ao estado exato
pré-restore (preciso, depende do regret existir); promover snapshot é um
rollback geral para o snapshot escolhido (não depende de regret).

### Nota de contrato

B expande o escopo do `doctor` (que era diagnóstico+reparo de `/boot`) para
incluir rollback de root. Isso é deliberado: a meta é o doctor resolver tudo. O
rollback continua executado pelo código do `restore` (regret/aside, asides
preservados), o doctor só roteia — sem duplicar a máquina de segurança.

## Plano incremental

```text
Fase 1 (feita) -> detectar + recusar + instruir
  - FSROOT de / vs subvol do fstab; divergência = resgate
  - caminho implícito recusa e instrui; --root/--boot explícitos fazem bypass
  - helpers puros com testes

Fase 2 -> verify: manual no resgate + testes do menu
  - menu unificado com default A (sync contra /@)
  - Opção A: auto-mount ro do /@ e sync de /boot
  - Opção B: handoff para o picker do restore (promover snapshot | Regret)

Fase 3 -> verify: invariante em teste + manual
  - antes de prompt_reboot(): subvol padrão casa com /boot
  - bloqueio + mensagem quando falha (backstop universal)
```

## Prompt para validação externa

Use o texto abaixo para pedir revisão crítica a outra IA.

```text
Estou desenvolvendo o snapgroup, uma ferramenta Rust que agrupa snapshots do
Snapper/BTRFS e faz restore pareado de subvolumes (root e home). O root fica em
BTRFS com subvol padrão /@. Em alguns sistemas /boot é uma partição FAT32
separada, fora do snapshot. Após restaurar um snapshot antigo, o snapgroup
sincroniza /boot (copia vmlinuz do root restaurado, regenera initramfs com
mkinitcpio, atualiza hashes BLAKE2B do Limine). Existe um subcomando
"snapg doctor" que diagnostica e repara /boot.

Incidente:
- Usuário restaurou para um checkpoint antigo (kernel 7.0.3-1). O /@ passou a
  ser o root antigo.
- O boot-sync pós-rollback foi interrompido; /boot ficou com kernel 7.0.10-2.
- No boot seguinte, o boot padrão (subvol=/@) caiu em Emergency Mode porque o
  kernel 7.0.10-2 de /boot não acha módulos no /@ (que só tem 7.0.3-1).
- Para recuperar, o usuário bootou pelo bootloader um SNAPSHOT de resgate
  (subvol @/.snapshots/660/snapshot, kernel 7.0.10-2) e rodou "snapg doctor".
- O doctor usa "/" como root alvo. Mas "/" era o snapshot de resgate, não o /@.
  Ele sincronizou /boot para 7.0.10-2 (kernel do resgate), piorando o estado.
- No reboot, o boot padrão (/@, 7.0.3-1) seguiu sem casar com /boot (7.0.10-2).

Diagnóstico:
- findmnt -no FSROOT / => @/.snapshots/660/snapshot (subvol montado em /)
- fstab, entrada "/" => subvol=/@ (subvol que boota por padrão)
- Quando esses dois divergem, "/" não é o root que vai bootar.

Proposta:
- snapg doctor (sem args) deve comparar o subvol montado em / com o subvol
  declarado no fstab (ou no cmdline da entrada padrão do bootloader). Se
  divergem, é boot de resgate e "/" é o root errado.
- Caminho explícito --root/--boot faz bypass da detecção.
- Opção 1: detectar, recusar e instruir o comando correto (mount -o subvol=/@
  <dev> /mnt/at && snapg doctor --root /mnt/at --boot /boot --apply).
- Opção 2: doctor monta o subvol padrão read-only e mira nele automaticamente,
  com cleanup garantido.
- Backstop: antes de oferecer reboot, verificar que o subvol que boota por
  padrão tem /usr/lib/modules/<kver> para o kernel em /boot.
- Fase 2 (menu unificado, doctor como resolvedor): em vez de só instruir, o
  doctor oferece um menu com default na opção não-destrutiva:
  - A (default): tornar bootável o sistema configurado (/@) — sync de /boot
    contra /@ (auto-mount read-only). Não toca no root.
  - B: mudar o que boota — handoff para o picker do restore, que já lista
    promover outro snapshot e desfazer a última restauração (Regret).
  Ponto crítico: B é sempre um rollback do root, nunca "sincronizar /boot para
  o snapshot montado" (isso recriaria o descasamento). Default é A porque é
  não-destrutiva e honra o restore que o usuário já fez.

Pergunta:
Comparar FSROOT de / contra o subvol do fstab é o sinal certo e robusto para
detectar "estou num snapshot de resgate"? Há caso em que fstab e cmdline do
bootloader discordam sobre o subvol padrão, e qual deveria vencer? A Opção 1
(recusar+instruir) é suficiente como primeira versão, ou a Opção 2 (auto-mount
read-only) é necessária para um tool de recuperação? No menu unificado da Fase
2, o default deve mesmo ser a opção não-destrutiva A (sync para o root que já
boota), ou há argumento para o default ser B (adotar o snapshot bootado)?
Expandir o doctor para também fazer rollback (via handoff ao restore) é um bom
contrato, ou o doctor deveria ficar restrito a /boot e só apontar o restore? O
invariante "subvol padrão casa com /boot" antes do reboot é o backstop certo,
ou há um sinal melhor? Existe abordagem mais simples ou mais robusta?

Responda como revisão técnica: achados primeiro, riscos, recomendação e plano
incremental de implementação.
```
