// Executed only by the separately approved installer helper. Device identity
// and the writable property are fixed; no browser-supplied path is accepted.
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Security.AccessControl;
using System.Security.Principal;
using System.Text;
public static class AmaterasuDisplayDeviceAccess {
    [StructLayout(LayoutKind.Sequential)]
    struct DeviceInfo { public uint Size; public Guid Class; public uint Instance; public UIntPtr Reserved; }
    [DllImport("setupapi.dll", CharSet=CharSet.Unicode, SetLastError=true)]
    static extern IntPtr SetupDiGetClassDevsW(IntPtr guid, string enumerator, IntPtr window, uint flags);
    [DllImport("setupapi.dll", SetLastError=true)]
    static extern bool SetupDiEnumDeviceInfo(IntPtr set, uint index, ref DeviceInfo info);
    [DllImport("setupapi.dll", CharSet=CharSet.Unicode, SetLastError=true)]
    static extern bool SetupDiGetDeviceRegistryPropertyW(IntPtr set, ref DeviceInfo info, uint property, out uint type, byte[] buffer, uint capacity, out uint required);
    [DllImport("setupapi.dll", CharSet=CharSet.Unicode, SetLastError=true)]
    static extern bool SetupDiSetDeviceRegistryPropertyW(IntPtr set, ref DeviceInfo info, uint property, byte[] buffer, uint size);
    [DllImport("setupapi.dll")]
    static extern bool SetupDiDestroyDeviceInfoList(IntPtr set);
    static string Property(IntPtr set, ref DeviceInfo info, uint property) {
        byte[] buffer=new byte[65536]; uint type,required;
        if (!SetupDiGetDeviceRegistryPropertyW(set,ref info,property,out type,buffer,(uint)buffer.Length,out required))
            throw new Win32Exception(Marshal.GetLastWin32Error());
        if (required>buffer.Length || (required&1)!=0) throw new InvalidOperationException("Invalid device property length");
        return Encoding.Unicode.GetString(buffer,0,(int)required).TrimEnd('\0');
    }
    public static string Merge(string original, string user) {
        SecurityIdentifier sid=new SecurityIdentifier(user);
        if (!user.StartsWith("S-1-5-21-",StringComparison.Ordinal)) throw new InvalidOperationException("Expected a runtime user SID");
        RawSecurityDescriptor descriptor=new RawSecurityDescriptor(original);
        if (descriptor.DiscretionaryAcl==null) throw new InvalidOperationException("The device must have an explicit DACL");
        int access=unchecked((int)0xc0000000);
        foreach (GenericAce entry in descriptor.DiscretionaryAcl) {
            CommonAce ace=entry as CommonAce;
            if (ace!=null && ace.AceQualifier==AceQualifier.AccessAllowed && ace.SecurityIdentifier.Equals(sid) &&
                (((ace.AccessMask&access)==access)||((ace.AccessMask&0x10000000)!=0)))
                return descriptor.GetSddlForm(AccessControlSections.Access);
        }
        descriptor.DiscretionaryAcl.InsertAce(descriptor.DiscretionaryAcl.Count,new CommonAce(AceFlags.None,AceQualifier.AccessAllowed,access,sid,false,null));
        return descriptor.GetSddlForm(AccessControlSections.Access);
    }
    public static bool Grant(string user) {
        IntPtr set=SetupDiGetClassDevsW(IntPtr.Zero,"ROOT",IntPtr.Zero,6); // PRESENT | ALLCLASSES
        if (set==new IntPtr(-1)) throw new Win32Exception(Marshal.GetLastWin32Error());
        try {
            uint matches=0; DeviceInfo selected=new DeviceInfo();
            for (uint index=0;;index++) {
                DeviceInfo info=new DeviceInfo();info.Size=(uint)Marshal.SizeOf(typeof(DeviceInfo));
                if (!SetupDiEnumDeviceInfo(set,index,ref info)) {
                    int error=Marshal.GetLastWin32Error();if (error==259) break;throw new Win32Exception(error);
                }
                string hardware;
                try {hardware=Property(set,ref info,1);} catch(Win32Exception error) {if(error.NativeErrorCode==13||error.NativeErrorCode==1168)continue;throw;}
                foreach (string identity in hardware.Split('\0')) {
                    if (String.Equals(identity,"Root\\SunshineVirtualDisplay",StringComparison.OrdinalIgnoreCase)) {selected=info;matches++;break;}
                }
            }
            if (matches!=1) throw new InvalidOperationException("Expected exactly one virtual display device");
            string before=Property(set,ref selected,24); // SPDRP_SECURITY_SDS
            string after=Merge(before,user);
            if (!String.Equals(before,after,StringComparison.Ordinal)) {
                byte[] value=Encoding.Unicode.GetBytes(after+"\0");
                if (!SetupDiSetDeviceRegistryPropertyW(set,ref selected,24,value,(uint)value.Length)) throw new Win32Exception(Marshal.GetLastWin32Error());
            }
            string confirmed=Property(set,ref selected,24);
            if (!String.Equals(Merge(confirmed,user),confirmed,StringComparison.Ordinal)) throw new InvalidOperationException("Device access grant did not persist");
            return !String.Equals(before,after,StringComparison.Ordinal);
        } finally {SetupDiDestroyDeviceInfoList(set);}
    }
}
