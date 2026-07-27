use std::io::{self, Write};
use std::path::Path;

use nexos_installer::disk::{
    FilesystemKind, GuidedMode, PartitionPlan, PartitionRole, TableKind, create_blank_bytes,
    empty_layout, format_partition, generate_guid, guided_layout, inspect_bytes, inspect_image,
    layout_from_report, read_image, require_confirmation, validate_image_path, write_layout,
    write_transactional,
};

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if let Err(error) = run(&arguments) {
        eprintln!("diskutil: {error}");
        std::process::exit(1);
    }
}

#[allow(clippy::too_many_lines)]
fn run(arguments: &[String]) -> Result<(), String> {
    match arguments {
        [] => interactive(),
        [command, image] if command == "inspect" || command == "list" => {
            print_report(image, &inspect_image(Path::new(image))?);
            Ok(())
        }
        [command, image, size, rest @ ..] if command == "create" => {
            let size = parse_usize(size, "size-MiB")?;
            let (yes, confirmation) = destructive_flags(rest)?;
            let path = Path::new(image);
            validate_image_path(path)?;
            require_confirmation(path, yes, confirmation)?;
            if path.exists() {
                return Err("refusing to overwrite an existing image with create".into());
            }
            let mut bytes = create_blank_bytes(size)?;
            let layout = empty_layout(bytes.len(), TableKind::Mbr)?;
            write_layout(&mut bytes, &layout)?;
            write_transactional(path, &bytes)?;
            println!("Created {size} MiB image: {image}");
            Ok(())
        }
        [command, image, mode, size, rest @ ..] if command == "guided" => {
            let mode = parse_mode(mode)?;
            let size = parse_usize(size, "size-MiB")?;
            let (yes, confirmation) = destructive_flags(rest)?;
            let path = Path::new(image);
            require_confirmation(path, yes, confirmation)?;
            let mut bytes = create_blank_bytes(size)?;
            let layout = guided_layout(size, mode)?;
            write_layout(&mut bytes, &layout)?;
            write_transactional(path, &bytes)?;
            print_report(image, &inspect_image(path)?);
            Ok(())
        }
        [command, image, table, rest @ ..] if command == "table" => {
            let kind = parse_table(table)?;
            let (yes, confirmation) = destructive_flags(rest)?;
            let path = Path::new(image);
            require_confirmation(path, yes, confirmation)?;
            let mut bytes = read_image(path)?;
            let layout = empty_layout(bytes.len(), kind)?;
            write_layout(&mut bytes, &layout)?;
            write_transactional(path, &bytes)?;
            print_report(image, &inspect_image(path)?);
            Ok(())
        }
        [command, image, role, first_lba, sectors, name, rest @ ..]
            if command == "partition-create" =>
        {
            let role = parse_role(role)?;
            let first_lba = parse_u64(first_lba, "first LBA")?;
            let sector_count = parse_u64(sectors, "sector count")?;
            let (yes, confirmation) = destructive_flags(rest)?;
            let path = Path::new(image);
            require_confirmation(path, yes, confirmation)?;
            let mut bytes = read_image(path)?;
            let report = inspect_bytes(&bytes)?;
            let mut layout = layout_from_report(&report);
            layout.partitions.push(PartitionPlan {
                role,
                name: name.clone(),
                first_lba,
                sector_count,
                unique_guid: generate_guid(),
                bootable: role == PartitionRole::EfiSystem && layout.kind == TableKind::Mbr,
            });
            write_layout(&mut bytes, &layout)?;
            write_transactional(path, &bytes)?;
            print_report(image, &inspect_image(path)?);
            Ok(())
        }
        [command, image, index, rest @ ..] if command == "partition-delete" => {
            let index = parse_u32(index, "partition index")?;
            let (yes, confirmation) = destructive_flags(rest)?;
            let path = Path::new(image);
            require_confirmation(path, yes, confirmation)?;
            let mut bytes = read_image(path)?;
            let report = inspect_bytes(&bytes)?;
            if !report.partitions.iter().any(|item| item.index == index) {
                return Err(format!("partition {index} does not exist"));
            }
            let mut layout = layout_from_report(&report);
            let position = usize::try_from(index.saturating_sub(1))
                .map_err(|_| "partition index is too large")?;
            if position >= layout.partitions.len() {
                return Err("sparse partition deletion is not supported".into());
            }
            layout.partitions.remove(position);
            write_layout(&mut bytes, &layout)?;
            write_transactional(path, &bytes)?;
            print_report(image, &inspect_image(path)?);
            Ok(())
        }
        [command, image, index, filesystem, rest @ ..] if command == "format" => {
            let index = parse_u32(index, "partition index")?;
            let filesystem = parse_filesystem(filesystem)?;
            let (yes, confirmation) = destructive_flags(rest)?;
            let path = Path::new(image);
            require_confirmation(path, yes, confirmation)?;
            let mut bytes = read_image(path)?;
            let report = inspect_bytes(&bytes)?;
            let partition = report
                .partitions
                .iter()
                .find(|partition| partition.index == index)
                .ok_or_else(|| format!("partition {index} does not exist"))?;
            format_partition(&mut bytes, partition, filesystem, generate_guid().0)?;
            write_transactional(path, &bytes)?;
            println!(
                "Formatted {image} partition {index} as {}",
                filesystem.name()
            );
            Ok(())
        }
        [command, image, index] if command == "check" => {
            let index = parse_u32(index, "partition index")?;
            let bytes = read_image(Path::new(image))?;
            let report = inspect_bytes(&bytes)?;
            let partition = report
                .partitions
                .iter()
                .find(|partition| partition.index == index)
                .ok_or_else(|| format!("partition {index} does not exist"))?;
            println!(
                "{image} partition {index}: {}",
                nexos_installer::disk::check_partition(&bytes, partition)?
            );
            Ok(())
        }
        _ => Err(usage()),
    }
}

fn interactive() -> Result<(), String> {
    println!("NexOS diskutil interactive image mode");
    println!("Physical disks are disabled in this release.");
    println!("1) Inspect image");
    println!("2) Create guided combined BIOS/UEFI layout");
    print!("Selection: ");
    io::stdout().flush().map_err(io_error)?;
    let selection = read_line()?;
    match selection.as_str() {
        "1" => {
            print!("Image path: ");
            io::stdout().flush().map_err(io_error)?;
            let image = read_line()?;
            print_report(&image, &inspect_image(Path::new(&image))?);
            Ok(())
        }
        "2" => {
            print!("New image path: ");
            io::stdout().flush().map_err(io_error)?;
            let image = read_line()?;
            print!("Size in MiB (minimum 96): ");
            io::stdout().flush().map_err(io_error)?;
            let size = parse_usize(&read_line()?, "size-MiB")?;
            println!("This will create and partition: {image}");
            print!("Type the complete target to confirm: ");
            io::stdout().flush().map_err(io_error)?;
            let confirmation = read_line()?;
            run(&[
                "guided".into(),
                image.clone(),
                "combined".into(),
                size.to_string(),
                "--yes".into(),
                "--confirm".into(),
                confirmation,
            ])
        }
        _ => Err("unknown menu selection".into()),
    }
}

fn print_report(image: &str, report: &nexos_installer::disk::DiskReport) {
    println!("Disk image: {image}");
    println!(
        "Capacity: {} MiB ({} sectors x {} bytes)",
        report.bytes / 1024 / 1024,
        report.sectors,
        report.sector_size
    );
    println!("Partition table: {:?}", report.table);
    if report.partitions.is_empty() {
        println!("No partitions.");
        return;
    }
    println!("Index  Start LBA  Sectors     Role        Filesystem  Boot  Name");
    for partition in &report.partitions {
        println!(
            "{:<6} {:<10} {:<11} {:<11?} {:<11} {:<5} {}",
            partition.index,
            partition.first_lba,
            partition.sector_count,
            partition.role,
            partition.filesystem.name(),
            if partition.bootable { "yes" } else { "no" },
            partition.name
        );
    }
}

fn destructive_flags(arguments: &[String]) -> Result<(bool, &str), String> {
    let mut yes = false;
    let mut confirmation = None;
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--yes" => {
                yes = true;
                index += 1;
            }
            "--confirm" => {
                confirmation = arguments.get(index + 1).map(String::as_str);
                index += 2;
            }
            value => return Err(format!("unknown destructive option: {value}")),
        }
    }
    Ok((yes, confirmation.unwrap_or("")))
}

fn parse_mode(value: &str) -> Result<GuidedMode, String> {
    match value {
        "combined" => Ok(GuidedMode::Combined),
        "uefi" => Ok(GuidedMode::Uefi),
        "bios" => Ok(GuidedMode::Bios),
        _ => Err("mode must be combined, uefi, or bios".into()),
    }
}

fn parse_table(value: &str) -> Result<TableKind, String> {
    match value {
        "gpt" => Ok(TableKind::Gpt),
        "mbr" => Ok(TableKind::Mbr),
        _ => Err("partition table must be gpt or mbr".into()),
    }
}

fn parse_role(value: &str) -> Result<PartitionRole, String> {
    match value {
        "bios" => Ok(PartitionRole::BiosBoot),
        "esp" => Ok(PartitionRole::EfiSystem),
        "nexfs" => Ok(PartitionRole::NexFs),
        "data" => Ok(PartitionRole::Data),
        _ => Err("role must be bios, esp, nexfs, or data".into()),
    }
}

fn parse_filesystem(value: &str) -> Result<FilesystemKind, String> {
    match value {
        "fat32" => Ok(FilesystemKind::Fat32),
        "nexfs" => Ok(FilesystemKind::NexFs),
        _ => Err("filesystem must be fat32 or nexfs".into()),
    }
}

fn parse_usize(value: &str, name: &str) -> Result<usize, String> {
    value
        .parse()
        .map_err(|_| format!("{name} must be an integer"))
}

fn parse_u64(value: &str, name: &str) -> Result<u64, String> {
    value
        .parse()
        .map_err(|_| format!("{name} must be an integer"))
}

fn parse_u32(value: &str, name: &str) -> Result<u32, String> {
    value
        .parse()
        .map_err(|_| format!("{name} must be an integer"))
}

fn read_line() -> Result<String, String> {
    let mut value = String::new();
    io::stdin().read_line(&mut value).map_err(io_error)?;
    Ok(value.trim().to_owned())
}

#[allow(clippy::needless_pass_by_value)]
fn io_error(error: io::Error) -> String {
    error.to_string()
}

fn usage() -> String {
    [
        "usage:",
        "  diskutil inspect <image>",
        "  diskutil create <image> <size-MiB> --yes --confirm <image>",
        "  diskutil guided <image> <combined|uefi|bios> <size-MiB> --yes --confirm <image>",
        "  diskutil table <image> <gpt|mbr> --yes --confirm <image>",
        "  diskutil partition-create <image> <bios|esp|nexfs|data> <first-LBA> <sectors> <name> --yes --confirm <image>",
        "  diskutil partition-delete <image> <index> --yes --confirm <image>",
        "  diskutil format <image> <index> <fat32|nexfs> --yes --confirm <image>",
        "  diskutil check <image> <index>",
        "",
        "Run without arguments for the interactive menu.",
        "Only ordinary image files are supported; raw physical disks are refused.",
    ]
    .join("\n")
}
