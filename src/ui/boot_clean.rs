pub(crate) fn print_cleanup_deferred() {
    println!("snapg boot-clean: outro snapg está rodando; cleanup adiado para o próximo boot");
}

pub(crate) fn print_disarm_failed(error: &anyhow::Error) {
    eprintln!("snapg boot-clean: falha ao desarmar serviço: {error:#}");
}

pub(crate) fn print_no_discards() {
    println!("snapg boot-clean: nenhum discard encontrado");
}

pub(crate) fn print_discard_removed(name: &str) {
    println!("snapg boot-clean: removido {name}");
}

pub(crate) fn print_discard_remove_failed(name: &str, error: &anyhow::Error) {
    eprintln!("snapg boot-clean: falha em {name}: {error:#}");
}

pub(crate) fn print_discard_summary(ok: usize, total: usize) {
    println!("snapg boot-clean: {ok}/{total} discards removidos");
}
