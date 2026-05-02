# Arquitetura de Restauração Interativa ("Regret Highlander")

Este documento descreve a nova arquitetura unificada para o gerenciamento de estados no `snapgroup`, substituindo os comandos fragmentados (`undo`, `redo`, `gc`) por uma interface de usuário interativa (TUI) centralizada no comando `restore`.

## 1. Princípios Fundamentais

*   **Comandos Simplificados:** O fluxo do usuário se resume a criar pontos (`save`) e voltar no tempo de forma visual (`restore`). O histórico é um "cardápio", não uma pilha de comandos para decorar.
*   **Regra "Highlander" (Apenas um Arrependimento):** O sistema suporta a existência de apenas **um** estado de rollback por vez, denominado internamente como `snapg_regret`.
    *   *Por que:* Elimina a necessidade de um *Garbage Collector* (`gc`) manual ou complexo. O acúmulo de lixo no sistema de arquivos BTRFS (subvolumes órfãos de undos sucessivos) deixa de existir.
*   **Save Mata o Regret:** A execução de um novo comando `save` sinaliza a aceitação da linha do tempo atual. Como consequência imediata, qualquer subvolume `snapg_regret` existente é deletado silenciosamente.

## 2. Fluxo de Vida do Sistema

### 2.1. Criação de Checkpoints (`save`)
A funcionalidade base do `save` permanece intacta. Ele cria snapshots consistentes agrupados por um `Checkpoint ID` (epoch) através de múltiplas configurações do Snapper.
*   *Adição:* Se existir um subvolume `_snapg_regret` no BTRFS, o `save` o deleta antes de criar o novo checkpoint.

### 2.2. A Interface Interativa (`restore`)
O comando `snapg restore` exibe uma TUI (Text User Interface) similar ao *Clonezilla*, permitindo seleção visual com setas do teclado e marcação com Espaço/Enter.

**Visualização Padrão:**
```text
[ ] Checkpoint 2 (2026-04-30 14:00 - Antes da atualização do Kernel)
[ ] Checkpoint 1 (2026-04-20 09:00 - Instalação base)
```

Se houver um `Regret` ativo (ou seja, o usuário já fez uma restauração e pode querer voltar atrás), ele aparece no topo:
```text
> [ ] Estado Anterior à Restauração (Regret)
  [x] Checkpoint 2 (2026-04-30 14:00) <-- [SISTEMA ATUAL]
  [ ] Checkpoint 1 (2026-04-20 09:00)
```

### 2.3. Executando uma Restauração (O Antigo 'Undo')
Se o usuário selecionar um Checkpoint anterior (ex: Checkpoint 1):
1.  O código arquiva o sistema montado atualmente como o novo e único `_snapg_regret` (sobrescrevendo/deletando qualquer regret anterior).
2.  Restaura os subvolumes do Checkpoint 1.
3.  Orienta o reboot.

### 2.4. Desfazendo uma Restauração (O Antigo 'Redo')
Se o usuário selecionar a opção `[ ] Estado Anterior à Restauração (Regret)`:
1.  O sistema atual (que era o Checkpoint 1 restaurado) é movido para o nome de descarte (`_snapg_discard_...`). *Nota: A mecânica do "Fantasma do Mount" permanece aqui.*
2.  O subvolume `_snapg_regret` é renomeado para ser o sistema principal (`@`, `@home`).
3.  Orienta o reboot.

## 3. Resolução de Casos Limite

### 3.1. A Maldição do "Snapshot Incompleto"
*Cenário:* Um Checkpoint possui snapshots de `@root` e `@home`, mas o usuário apagou manualmente o snapshot do `@home` usando comandos nativos do snapper.
*Abordagem:*
*   A lista principal exibe o Checkpoint normalmente.
*   **Expansão Opcional (TUI Avançada):** Ao pressionar `Shift+Enter` (ou uma tecla designada) sobre um Checkpoint, a TUI expande para mostrar os membros daquele ponto no tempo.
    ```text
    v Checkpoint 2 (2026-04-30 14:00)
        [x] / (root) - Disponível
        [ ] /home - (Snapshot não encontrado)
    ```
*   *Execução:* A restauração atua no modelo *best-effort*, aplicando o rollback apenas nos membros disponíveis ou selecionados explicitamente pelo usuário na visão expandida.

### 3.2. O Fantasma do Mount (Descarte Adiado)
*Problema:* O BTRFS impede a deleção imediata do subvolume que está montado como `/` (Device or resource busy).
*Abordagem (Mantida da implementação atual):*
*   O sistema não tenta deletar o subvolume ativo durante o comando `restore` para o `Regret`.
*   O subvolume atual é movido para um nome temporário de descarte (ex: `_snapg_discard_lixo`).
*   Um serviço do systemd (`snapg-cleanup.service`), já validado, cuida de varrer e deletar automaticamente os `discards` no próximo boot, antes mesmo do usuário perceber.

## 4. Plano de Implementação

1.  **Dependências:** Adicionar um crate para TUI (como `dialoguer` ou `inquire`) para desenhar as listas de seleção e capturar teclas interativas.
2.  **Limpeza de Código:**
    *   Remover `cli::Command::Undo`, `cli::Command::Redo`, `cli::Command::Gc`.
    *   Criar `cli::Command::Restore`.
3.  **Refatoração de Ciclo de Vida:**
    *   Atualizar o `save` para deletar `_snapg_regret`.
    *   Modificar o motor de rollback para suportar o fluxo "Regret Highlander" e a restauração parcial através do input do usuário.
