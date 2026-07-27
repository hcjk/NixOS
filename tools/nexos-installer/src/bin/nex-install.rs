use std::io::{self, Write};
use std::path::PathBuf;

use nexos_installer::disk::GuidedMode;
use nexos_installer::install::{InstallMode, InstallRequest, install, verify_installed_image};

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if let Err(error) = run(&arguments) {
        eprintln!("nex-install: {error}");
        std::process::exit(1);
    }
}

fn run(arguments: &[String]) -> Result<(), String> {
    if arguments.is_empty() {
        return interactive();
    }
    if arguments
        .first()
        .is_some_and(|argument| argument == "verify")
    {
        let target = arguments
            .get(1)
            .ok_or("usage: nex-install verify <image>")?;
        if arguments.len() != 2 {
            return Err("usage: nex-install verify <image>".into());
        }
        let report = verify_installed_image(&PathBuf::from(target))?;
        println!(
            "Verified NexOS installation on {}: ESP p{}, root p{}, UUID={}, files={}",
            report.target.display(),
            report.esp_partition,
            report.root_partition,
            nexos_installer::disk::hex_guid(nexos_storage::Guid(report.root_uuid)),
            report.verified_files
        );
        return Ok(());
    }
    let parsed = ParsedArguments::parse(arguments)?;
    println!("NexOS installation summary");
    println!("  target: {}", parsed.target.display());
    println!("  mode: {}", parsed.mode.name());
    println!("  kernel: {}", parsed.kernel.display());
    println!("  Limine: {}", parsed.limine.display());
    if let InstallMode::Guided(_) = parsed.mode {
        println!("  target size: {} MiB", parsed.size_mib);
    }
    println!("  action: format selected partitions and install NexOS");
    let report = install(&InstallRequest {
        target: parsed.target,
        kernel: parsed.kernel,
        limine_directory: parsed.limine,
        size_mib: parsed.size_mib,
        mode: parsed.mode,
        yes: parsed.yes,
        confirmation: parsed.confirmation,
        install_bios_stage: true,
    })?;
    print_success(&report);
    Ok(())
}

struct ParsedArguments {
    target: PathBuf,
    kernel: PathBuf,
    limine: PathBuf,
    size_mib: usize,
    mode: InstallMode,
    yes: bool,
    confirmation: String,
}

impl ParsedArguments {
    fn parse(arguments: &[String]) -> Result<Self, String> {
        let mut target = None;
        let mut kernel = None;
        let mut limine = None;
        let mut size_mib = 128;
        let mut guided_mode = GuidedMode::Combined;
        let mut manual = false;
        let mut esp_partition = None;
        let mut root_partition = None;
        let mut manual_bios = false;
        let mut yes = false;
        let mut confirmation = String::new();
        let mut index = 0;
        while index < arguments.len() {
            match arguments[index].as_str() {
                "--target" => {
                    target = Some(PathBuf::from(value(arguments, index, "--target")?));
                    index += 2;
                }
                "--kernel" => {
                    kernel = Some(PathBuf::from(value(arguments, index, "--kernel")?));
                    index += 2;
                }
                "--limine" => {
                    limine = Some(PathBuf::from(value(arguments, index, "--limine")?));
                    index += 2;
                }
                "--size-mib" => {
                    size_mib = value(arguments, index, "--size-mib")?
                        .parse()
                        .map_err(|_| "--size-mib must be an integer")?;
                    index += 2;
                }
                "--mode" => {
                    guided_mode = match value(arguments, index, "--mode")? {
                        "combined" => GuidedMode::Combined,
                        "uefi" => GuidedMode::Uefi,
                        "bios" => GuidedMode::Bios,
                        _ => return Err("--mode must be combined, uefi, or bios".into()),
                    };
                    index += 2;
                }
                "--manual" => {
                    manual = true;
                    index += 1;
                }
                "--esp" => {
                    esp_partition = Some(
                        value(arguments, index, "--esp")?
                            .parse()
                            .map_err(|_| "--esp must be a partition index")?,
                    );
                    index += 2;
                }
                "--root" => {
                    root_partition = Some(
                        value(arguments, index, "--root")?
                            .parse()
                            .map_err(|_| "--root must be a partition index")?,
                    );
                    index += 2;
                }
                "--bios" => {
                    manual_bios = true;
                    index += 1;
                }
                "--yes" => {
                    yes = true;
                    index += 1;
                }
                "--confirm" => {
                    value(arguments, index, "--confirm")?.clone_into(&mut confirmation);
                    index += 2;
                }
                "--help" | "-h" => return Err(usage()),
                argument => return Err(format!("unknown option: {argument}\n\n{}", usage())),
            }
        }
        let mode = if manual {
            InstallMode::Manual {
                esp_partition: esp_partition.ok_or("--manual requires --esp <index>")?,
                root_partition: root_partition.ok_or("--manual requires --root <index>")?,
                bios: manual_bios,
            }
        } else {
            InstallMode::Guided(guided_mode)
        };
        Ok(Self {
            target: target.ok_or("--target is required")?,
            kernel: kernel.ok_or("--kernel is required")?,
            limine: limine.ok_or("--limine is required")?,
            size_mib,
            mode,
            yes,
            confirmation,
        })
    }
}

fn interactive() -> Result<(), String> {
    println!("NexOS interactive installer");
    println!("This release installs only to ordinary disk-image files.");
    let target = prompt("Target image path: ")?;
    let kernel = prompt("NexOS kernel ELF path: ")?;
    let limine = prompt("Limine directory: ")?;
    let size = prompt("Image size in MiB [128]: ")?;
    let size_mib = if size.is_empty() {
        128
    } else {
        size.parse().map_err(|_| "size must be an integer")?
    };
    println!();
    println!("DESTRUCTIVE SUMMARY");
    println!("  create or replace: {target}");
    println!("  layout: combined GPT BIOS + UEFI");
    println!("  size: {size_mib} MiB");
    let confirmation = prompt("Type the complete target path to continue: ")?;
    let report = install(&InstallRequest {
        target: PathBuf::from(&target),
        kernel: PathBuf::from(kernel),
        limine_directory: PathBuf::from(limine),
        size_mib,
        mode: InstallMode::Guided(GuidedMode::Combined),
        yes: true,
        confirmation,
        install_bios_stage: true,
    })?;
    print_success(&report);
    Ok(())
}

fn print_success(report: &nexos_installer::install::InstallReport) {
    println!();
    println!("NexOS installation completed and verified.");
    println!("  target: {}", report.target.display());
    println!("  EFI/boot partition: p{}", report.esp_partition);
    println!("  NexFS root partition: p{}", report.root_partition);
    println!(
        "  root UUID: {}",
        nexos_installer::disk::hex_guid(nexos_storage::Guid(report.root_uuid))
    );
    println!("  verified files: {}", report.verified_files);
    println!("  BIOS stage installed: {}", report.bios_stage_installed);
    println!("Remove the installation medium and reboot from the installed image.");
}

fn value<'a>(arguments: &'a [String], index: usize, option: &str) -> Result<&'a str, String> {
    arguments
        .get(index + 1)
        .map(String::as_str)
        .ok_or_else(|| format!("{option} requires a value"))
}

fn prompt(label: &str) -> Result<String, String> {
    print!("{label}");
    io::stdout().flush().map_err(|error| error.to_string())?;
    let mut value = String::new();
    io::stdin()
        .read_line(&mut value)
        .map_err(|error| error.to_string())?;
    Ok(value.trim().to_owned())
}

fn usage() -> String {
    [
        "usage:",
        "  nex-install --target <image> --kernel <elf> --limine <dir>",
        "              [--size-mib 128] [--mode combined|uefi|bios]",
        "              --yes --confirm <image>",
        "  nex-install --manual --target <image> --esp <index> --root <index>",
        "              [--bios] --kernel <elf> --limine <dir>",
        "              --yes --confirm <image>",
        "  nex-install verify <image>",
        "",
        "Run without arguments for guided interactive installation.",
        "Raw physical disks are deliberately refused in this release.",
    ]
    .join("\n")
}
