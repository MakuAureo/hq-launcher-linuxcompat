import { getCurrentWindow } from '@tauri-apps/api/window';
import { useEffect, useState } from 'react';
import { Minus, Square, X } from 'lucide-react';
import * as DropdownMenu from '@radix-ui/react-dropdown-menu';
import { cn } from './lib/cn';
import { invoke } from '@tauri-apps/api/core';
import { register } from '@tauri-apps/plugin-global-shortcut';


export default function Titlebar({ ...props }) {
    const [isMaximized, setIsMaximized] = useState(false);
    
    const appWindow = getCurrentWindow();
    useEffect(() => {
        const handleResize = async () => {
            const maximized = await appWindow.isMaximized();
            setIsMaximized(maximized);
        };

        handleResize();
        const unlisten = appWindow.onResized(() => {
            handleResize();
        });

        return () => {
            unlisten.then(fn => fn());
        };
    }, []);
    const handleMinimize = () => appWindow.minimize();
    const handleMaximize = () => {
        appWindow.toggleMaximize();
        setIsMaximized(!isMaximized);
    };
    const handleClose = () => {
        console.log('close');
        appWindow.close()};



    return (
        <div 
            data-tauri-drag-region 
            className={cn('w-full flex items-center justify-between px-2 border-b border-white/10 bg-[#0b0c10]/80 backdrop-blur-sm z-50', props.className)}
        >
            {/* Left side - Menu items */}
            <div className="flex items-center gap-1">
                <img src="/icon.svg" alt="logo" className='ml-2 w-4 h-4' />
                <TitlebarMenu 
                    name="File" 
                    items={[
                        // { label: "Open App Settings", shortcut: "CommandOrControl+,", action: () => console.log('Open settings') },
                        { label: "Open Version Folder", shortcut: "CommandOrControl+O", action: () => {invoke('open_version_folder')} }
                    ]} 
                />
            </div>

            {/* Right side - Window controls */}
            <div className="flex items-center" data-tauri-drag-region="false">
                <button
                    onClick={handleMinimize}
                    className="w-8 h-8 flex items-center justify-center hover:bg-white/10 transition-colors rounded-sm"
                    aria-label="Minimize"
                >
                    <Minus size={14} className="text-white/80" />
                </button>
                <button
                    onClick={handleMaximize}
                    className="w-8 h-8 flex items-center justify-center hover:bg-white/10 transition-colors rounded-sm"
                    aria-label={isMaximized ? "Restore" : "Maximize"}
                >
                    <Square size={12} className="text-white/80" />
                </button>
                <button
                    onClick={handleClose}
                    className="w-8 h-8 flex items-center justify-center hover:bg-red-500/20 transition-colors rounded-sm"
                    aria-label="Close"
                >
                    <X size={14} className="text-white/80" />
                </button>
            </div>
        </div>
    );
}

function TitlebarMenu({ name, items }) {
    let registeredShortcuts = [];


    useEffect(() => {
        items.forEach(item => {
            if (item.shortcut) {
                if (registeredShortcuts.includes(item.shortcut)) { return }
                register(item.shortcut, () => {
                    item.action?.();
                    console.log('shortcut registered', item.shortcut);
                })
            }
        });
    }, [items]);
    return (
        <DropdownMenu.Root>
            <DropdownMenu.Trigger asChild>
                <button
                    data-tauri-drag-region="false"
                    className={cn(
                        "text-xs font-light text-white/90 px-2 py-1 hover:bg-white/10 rounded transition-colors outline-none",
                        "data-[state=open]:bg-white/10"
                    )}
                >
                    {name}
                </button>
            </DropdownMenu.Trigger>

            <DropdownMenu.Portal>
                <DropdownMenu.Content
                    className={cn(
                        "min-w-[200px] bg-[#1a1b23] border border-white/10 rounded-md shadow-lg py-1 z-50",
                        "data-[state=open]:animate-in data-[state=closed]:animate-out",
                        "data-[state=closed]:fade-out-0 data-[state=open]:fade-in-0",
                        "data-[state=closed]:zoom-out-95 data-[state=open]:zoom-in-95",
                        "data-[side=bottom]:slide-in-from-top-2"
                    )}
                    sideOffset={4}
                    align="start"
                >
                    {items.map((item, index) => (
                        <DropdownMenu.Item
                            key={index}
                            onSelect={() => {
                                item.action?.();
                            }}
                            className={cn(
                                "w-full text-left text-xs text-white/90 px-3 py-1.5",
                                "hover:bg-white/10 transition-colors outline-none cursor-pointer",
                                "flex items-center justify-between",
                                "focus:bg-white/10"
                            )}
                        >
                            <span>{item.label}</span>
                            {item.shortcut && (
                                <span className="text-xs text-white/50 ml-4">{invoke('get_global_shortcut', { shortcut: item.shortcut })}</span>
                            )}
                        </DropdownMenu.Item>
                    ))}
                </DropdownMenu.Content>
            </DropdownMenu.Portal>
        </DropdownMenu.Root>
    );
}
